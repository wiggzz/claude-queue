use crate::backend::AgentBackend;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How to interpret the pattern field when matching tool input.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    /// Match the pattern as a regex against the extracted text (default).
    #[default]
    Regex,
    /// Match the pattern as a domain against the URL's host (for WebFetch).
    Domain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub tool: String,
    pub action: String, // "allow", "deny", "ask"
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>, // regex pattern to match against tool_input (e.g. Bash command)
    #[serde(default)]
    #[serde(skip_serializing_if = "is_default_match_mode")]
    pub match_mode: MatchMode,
}

fn is_default_match_mode(mode: &MatchMode) -> bool {
    *mode == MatchMode::Regex
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SupervisorConfig {
    pub enabled: bool,
    pub rules: Vec<String>,
    /// Whether to include the agent's session prompt/task in the supervisor context.
    /// Default: false. When false, the supervisor evaluates tool calls purely on their
    /// own merit, preventing the agent's prompt from biasing approval decisions.
    #[serde(default)]
    pub include_session_context: bool,
    #[serde(default)]
    pub backends: BackendConfigs,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BackendConfig {
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BackendConfigs {
    #[serde(default)]
    pub claude: BackendConfig,
    #[serde(default)]
    pub pi: BackendConfig,
}

impl BackendConfigs {
    pub fn for_backend(&self, backend: AgentBackend) -> &BackendConfig {
        match backend {
            AgentBackend::Claude => &self.claude,
            AgentBackend::Pi => &self.pi,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DbLocation {
    #[default]
    User,
    ProjectLocal,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DbConfig {
    #[serde(default)]
    pub location: DbLocation,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct RawSupervisorConfig {
    pub enabled: Option<bool>,
    #[serde(default)]
    pub rules: Vec<String>,
    pub include_session_context: Option<bool>,
    #[serde(default)]
    pub backends: RawBackendConfigs,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct RawBackendConfig {
    pub model: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct RawBackendConfigs {
    #[serde(default)]
    pub claude: RawBackendConfig,
    #[serde(default)]
    pub pi: RawBackendConfig,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct RawDbConfig {
    pub location: Option<DbLocation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub timeout: u64,
    pub poll_interval: f64,
    pub default_backend: AgentBackend,
    #[serde(default)]
    pub backends: BackendConfigs,
    #[serde(default)]
    pub db: DbConfig,
    pub policies: Vec<Policy>,
    pub supervisor: SupervisorConfig,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct RawConfig {
    pub timeout: Option<u64>,
    pub poll_interval: Option<f64>,
    pub default_backend: Option<AgentBackend>,
    #[serde(default)]
    pub backends: RawBackendConfigs,
    #[serde(default)]
    pub db: RawDbConfig,
    #[serde(default)]
    pub policies: Vec<Policy>,
    #[serde(default)]
    pub supervisor: RawSupervisorConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            timeout: default_timeout(),
            poll_interval: default_poll_interval(),
            default_backend: AgentBackend::default(),
            backends: BackendConfigs::default(),
            db: DbConfig::default(),
            policies: Vec::new(),
            supervisor: SupervisorConfig::default(),
        }
    }
}

fn default_timeout() -> u64 {
    86400 // 24 hours
}

fn default_poll_interval() -> f64 {
    0.5
}

impl Config {
    /// Load and merge user config (~/.cq/config.json) and project config (.cq/config.json).
    /// Project policies come first (higher priority, first-match-wins).
    /// Claude Code permission settings are appended as lowest-priority fallback policies.
    pub fn load(project_dir: &Path) -> Self {
        Self::load_from_raw_paths(
            &user_config_path(),
            &project_config_path(project_dir),
            project_dir,
        )
    }

    fn load_single(path: &Path) -> Self {
        let raw = Self::load_single_raw(path);
        Config {
            timeout: raw.timeout.unwrap_or_else(default_timeout),
            poll_interval: raw.poll_interval.unwrap_or_else(default_poll_interval),
            default_backend: raw.default_backend.unwrap_or_default(),
            backends: load_backends(&raw.backends),
            db: load_db(&raw.db),
            policies: raw.policies,
            supervisor: SupervisorConfig {
                enabled: raw.supervisor.enabled.unwrap_or(false),
                rules: raw.supervisor.rules,
                include_session_context: raw.supervisor.include_session_context.unwrap_or(false),
                backends: load_backends(&raw.supervisor.backends),
            },
        }
    }

    fn load_single_raw(path: &Path) -> RawConfig {
        match fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => RawConfig::default(),
        }
    }

    /// Save config to a specific file path.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)
    }

    pub fn model_for_backend(&self, backend: AgentBackend) -> &str {
        &self.backends.for_backend(backend).model
    }

    fn load_from_raw_paths(user_path: &Path, project_path: &Path, project_dir: &Path) -> Self {
        let user_config = Self::load_single_raw(user_path);
        let project_config = Self::load_single_raw(project_path);

        let timeout = project_config
            .timeout
            .or(user_config.timeout)
            .unwrap_or_else(default_timeout);

        let poll_interval = project_config
            .poll_interval
            .or(user_config.poll_interval)
            .unwrap_or_else(default_poll_interval);

        let backends = merge_backends(&project_config.backends, &user_config.backends);
        let db = merge_db(&project_config.db, &user_config.db);

        let mut policies = project_config.policies.clone();
        policies.extend(user_config.policies.clone());

        let cc_policies = derive_claude_code_policies(project_dir);
        policies.extend(cc_policies);

        let supervisor = SupervisorConfig {
            enabled: project_config
                .supervisor
                .enabled
                .or(user_config.supervisor.enabled)
                .unwrap_or(false),
            rules: {
                let mut rules = project_config.supervisor.rules.clone();
                rules.extend(user_config.supervisor.rules.clone());
                rules
            },
            include_session_context: project_config
                .supervisor
                .include_session_context
                .or(user_config.supervisor.include_session_context)
                .unwrap_or(false),
            backends: merge_backends(
                &project_config.supervisor.backends,
                &user_config.supervisor.backends,
            ),
        };

        Config {
            timeout,
            poll_interval,
            default_backend: project_config
                .default_backend
                .or(user_config.default_backend)
                .unwrap_or_default(),
            backends,
            db,
            policies,
            supervisor,
        }
    }
}

fn load_backends(raw: &RawBackendConfigs) -> BackendConfigs {
    BackendConfigs {
        claude: BackendConfig {
            model: raw.claude.model.clone().unwrap_or_default(),
        },
        pi: BackendConfig {
            model: raw.pi.model.clone().unwrap_or_default(),
        },
    }
}

fn merge_backends(project: &RawBackendConfigs, user: &RawBackendConfigs) -> BackendConfigs {
    BackendConfigs {
        claude: BackendConfig {
            model: project
                .claude
                .model
                .clone()
                .or(user.claude.model.clone())
                .unwrap_or_default(),
        },
        pi: BackendConfig {
            model: project
                .pi
                .model
                .clone()
                .or(user.pi.model.clone())
                .unwrap_or_default(),
        },
    }
}

fn load_db(raw: &RawDbConfig) -> DbConfig {
    DbConfig {
        location: raw.location.unwrap_or_default(),
    }
}

fn merge_db(project: &RawDbConfig, user: &RawDbConfig) -> DbConfig {
    DbConfig {
        location: project.location.or(user.location).unwrap_or_default(),
    }
}

/// Load a single config file (for editing project or user config directly).
pub fn load_file(path: &Path) -> Config {
    Config::load_single(path)
}

pub(crate) fn load_file_raw(path: &Path) -> RawConfig {
    Config::load_single_raw(path)
}

pub fn user_config_path() -> PathBuf {
    dirs_home().join(".cq").join("config.json")
}

pub fn project_config_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".cq").join("config.json")
}

pub fn db_path() -> PathBuf {
    // Allow override via env var (used by hook to find the DB)
    if let Ok(path) = std::env::var("CQ_DB") {
        return PathBuf::from(path);
    }

    let project_dir = std::env::var("CQ_PROJECT_DIR")
        .map(PathBuf::from)
        .or_else(|_| std::env::current_dir())
        .unwrap_or_else(|_| PathBuf::from("."));
    db_path_for(&resolve_project_dir(&project_dir))
}

pub fn db_path_for(project_dir: &Path) -> PathBuf {
    if let Ok(path) = std::env::var("CQ_DB") {
        return PathBuf::from(path);
    }

    match Config::load(project_dir).db.location {
        DbLocation::User => dirs_home().join(".cq").join("cq.db"),
        DbLocation::ProjectLocal => project_dir.join(".cq").join("cq.db"),
    }
}

pub fn log_dir() -> PathBuf {
    dirs_home().join(".cq").join("logs")
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

pub fn resolve_project_dir(cwd: &Path) -> PathBuf {
    if cwd.join(".cq").join("config.json").exists() {
        return cwd.to_path_buf();
    }

    for ancestor in cwd.ancestors().skip(1) {
        if ancestor.join(".cq").join("config.json").exists() {
            return ancestor.to_path_buf();
        }
    }

    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(cwd)
        .output()
        && output.status.success()
    {
        let common_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let common_path = Path::new(&common_dir);
        if let Some(root) = common_path.parent() {
            return root.to_path_buf();
        }
    }

    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        && output.status.success()
    {
        let toplevel = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !toplevel.is_empty() {
            return PathBuf::from(toplevel);
        }
    }

    cwd.to_path_buf()
}

/// Derive cq policies from Claude Code permission settings.
///
/// Reads settings from (in order, project settings taking priority):
/// - `~/.claude/settings.json` (user-level)
/// - `.claude/settings.json` (project-level)
/// - `.claude/settings.local.json` (project-level local overrides)
///
/// Returns policies with project-level first (higher priority), then user-level.
fn derive_claude_code_policies(project_dir: &Path) -> Vec<Policy> {
    // Project-level settings first (higher priority)
    let project_local = project_dir.join(".claude").join("settings.local.json");
    let project_settings = project_dir.join(".claude").join("settings.json");
    // User-level settings last (lowest priority)
    let user_settings = dirs_home().join(".claude").join("settings.json");

    derive_policies_from_settings_files(&[project_local, project_settings, user_settings])
}

/// Extract policies from a list of Claude Code settings files (first file = highest priority).
fn derive_policies_from_settings_files(paths: &[PathBuf]) -> Vec<Policy> {
    let mut policies = Vec::new();

    for path in paths {
        if let Ok(contents) = fs::read_to_string(path)
            && let Ok(val) = serde_json::from_str::<serde_json::Value>(&contents)
            && let Some(perms) = val.get("permissions")
        {
            if let Some(allow) = perms.get("allow").and_then(|v| v.as_array()) {
                for entry in allow {
                    if let Some(s) = entry.as_str()
                        && let Some(policy) = parse_claude_code_permission(s, "allow")
                    {
                        policies.push(policy);
                    }
                }
            }
            if let Some(deny) = perms.get("deny").and_then(|v| v.as_array()) {
                for entry in deny {
                    if let Some(s) = entry.as_str()
                        && let Some(policy) = parse_claude_code_permission(s, "deny")
                    {
                        policies.push(policy);
                    }
                }
            }
        }
    }

    policies
}

/// Parse a single Claude Code permission entry like "Bash(cargo test:*)" or "Edit"
/// into a cq Policy. Returns None if the entry can't be parsed.
fn parse_claude_code_permission(entry: &str, action: &str) -> Option<Policy> {
    let entry = entry.trim();
    if entry.is_empty() {
        return None;
    }

    // Check for "ToolName(pattern)" format
    if let Some(paren_start) = entry.find('(') {
        let tool = &entry[..paren_start];
        if tool.is_empty() {
            return None;
        }
        // Extract pattern between parentheses
        let rest = &entry[paren_start + 1..];
        let pattern_str = rest.strip_suffix(')')?;

        // Handle WebFetch(domain:X) — domain matching mode
        if tool == "WebFetch"
            && let Some(domain) = pattern_str.strip_prefix("domain:")
        {
            return Some(Policy {
                tool: tool.to_string(),
                action: action.to_string(),
                pattern: Some(domain.to_string()),
                match_mode: MatchMode::Domain,
            });
        }

        // For path-based tools, strip leading // (Claude Code's absolute path convention)
        let pattern_str = if matches!(tool, "Read" | "Edit" | "Write" | "Glob" | "Grep")
            && pattern_str.starts_with("//")
        {
            &pattern_str[1..] // "//Users/..." → "/Users/..."
        } else {
            pattern_str
        };

        // Convert Claude Code glob pattern to regex
        let regex_pattern = claude_code_pattern_to_regex(pattern_str);

        Some(Policy {
            tool: tool.to_string(),
            action: action.to_string(),
            pattern: Some(regex_pattern),
            match_mode: MatchMode::Regex,
        })
    } else {
        // Bare tool name — allow/deny unconditionally
        Some(Policy {
            tool: entry.to_string(),
            action: action.to_string(),
            pattern: None,
            match_mode: MatchMode::Regex,
        })
    }
}

/// Convert a Claude Code glob-style pattern to a regex pattern.
/// Claude Code patterns use:
///   - `*` / `**` for wildcard matching
///   - `:` before `*` as a prefix separator (e.g. `cq start:*` matches "cq start anything")
fn claude_code_pattern_to_regex(pattern: &str) -> String {
    let mut regex = String::from("(?s)^"); // anchored, dot-matches-newline for ** patterns
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '*' {
            // Consume all consecutive *s (** and * both become .*)
            while i < chars.len() && chars[i] == '*' {
                i += 1;
            }
            regex.push_str(".*");
        } else if chars[i] == ':' && i + 1 < chars.len() && chars[i + 1] == '*' {
            // `:*` is Claude Code's prefix-match separator — skip the colon,
            // let the next iteration handle `*` → `.*`
            i += 1;
        } else if "\\^$.|?+()[]{}".contains(chars[i]) {
            // Escape regex special characters
            regex.push('\\');
            regex.push(chars[i]);
            i += 1;
        } else {
            regex.push(chars[i]);
            i += 1;
        }
    }

    regex
}

/// Create default user config if it doesn't exist.
pub fn ensure_user_config() {
    let path = user_config_path();
    if !path.exists() {
        let config = Config {
            timeout: default_timeout(),
            poll_interval: default_poll_interval(),
            default_backend: AgentBackend::default(),
            backends: BackendConfigs::default(),
            db: DbConfig::default(),
            policies: vec![
                Policy {
                    tool: "Read".into(),
                    action: "allow".into(),
                    pattern: None,
                    match_mode: MatchMode::default(),
                },
                Policy {
                    tool: "Glob".into(),
                    action: "allow".into(),
                    pattern: None,
                    match_mode: MatchMode::default(),
                },
                Policy {
                    tool: "Grep".into(),
                    action: "allow".into(),
                    pattern: None,
                    match_mode: MatchMode::default(),
                },
                Policy {
                    tool: "LS".into(),
                    action: "allow".into(),
                    pattern: None,
                    match_mode: MatchMode::default(),
                },
                Policy {
                    tool: "LSP".into(),
                    action: "allow".into(),
                    pattern: None,
                    match_mode: MatchMode::default(),
                },
                Policy {
                    tool: "ToolSearch".into(),
                    action: "allow".into(),
                    pattern: None,
                    match_mode: MatchMode::default(),
                },
            ],
            supervisor: SupervisorConfig {
                enabled: true,
                rules: Vec::new(),
                include_session_context: false,
                backends: BackendConfigs::default(),
            },
        };
        let _ = config.save(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_default_config() {
        let c = Config::default();
        assert_eq!(c.timeout, 86400);
        assert!((c.poll_interval - 0.5).abs() < f64::EPSILON);
        assert_eq!(c.default_backend, AgentBackend::Claude);
        assert_eq!(c.db.location, DbLocation::User);
        assert!(c.policies.is_empty());
        assert!(!c.supervisor.enabled);
        assert_eq!(c.backends.claude.model, "");
        assert_eq!(c.backends.pi.model, "");
        assert_eq!(c.supervisor.backends.claude.model, "");
        assert_eq!(c.supervisor.backends.pi.model, "");
    }

    #[test]
    fn test_load_missing_file() {
        let c = Config::load_single(Path::new("/tmp/nonexistent_cq_config_test.json"));
        assert_eq!(c.timeout, 86400);
        assert!((c.poll_interval - 0.5).abs() < f64::EPSILON);
        assert_eq!(c.default_backend, AgentBackend::Claude);
        assert!(c.policies.is_empty());
    }

    #[test]
    fn test_load_valid_config() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, r#"{{"timeout": 300, "poll_interval": 2.0, "default_backend": "pi", "backends": {{"claude": {{"model": "sonnet"}}, "pi": {{"model": "openai/gpt-5.4"}}}}, "policies": [{{"tool": "Bash", "action": "deny"}}], "supervisor": {{"enabled": true, "backends": {{"claude": {{"model": "haiku"}}, "pi": {{"model": "openai/gpt-5.4"}}}}, "rules": ["no secrets"]}}}}"#).unwrap();
        let c = Config::load_single(f.path());
        assert_eq!(c.timeout, 300);
        assert!((c.poll_interval - 2.0).abs() < f64::EPSILON);
        assert_eq!(c.default_backend, AgentBackend::Pi);
        assert_eq!(c.backends.claude.model, "sonnet");
        assert_eq!(c.backends.pi.model, "openai/gpt-5.4");
        assert_eq!(c.policies.len(), 1);
        assert_eq!(c.policies[0].tool, "Bash");
        assert_eq!(c.policies[0].action, "deny");
        assert!(c.supervisor.enabled);
        assert_eq!(c.supervisor.backends.claude.model, "haiku");
        assert_eq!(c.supervisor.backends.pi.model, "openai/gpt-5.4");
        assert_eq!(c.supervisor.rules, vec!["no secrets"]);
    }

    #[test]
    fn test_load_invalid_json() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "not json at all {{{{").unwrap();
        let c = Config::load_single(f.path());
        assert_eq!(c.timeout, 86400);
        assert!(c.policies.is_empty());
    }

    #[test]
    fn test_save_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let config = Config {
            timeout: 999,
            poll_interval: 1.5,
            default_backend: AgentBackend::Pi,
            backends: BackendConfigs {
                claude: BackendConfig {
                    model: "sonnet".into(),
                },
                pi: BackendConfig {
                    model: "openai/gpt-5.4".into(),
                },
            },
            db: DbConfig {
                location: DbLocation::ProjectLocal,
            },
            policies: vec![Policy {
                tool: "Write".into(),
                action: "ask".into(),
                pattern: Some("secret".into()),
                match_mode: MatchMode::default(),
            }],
            supervisor: SupervisorConfig {
                enabled: true,
                rules: vec!["be safe".into()],
                include_session_context: false,
                backends: BackendConfigs {
                    claude: BackendConfig {
                        model: "haiku".into(),
                    },
                    pi: BackendConfig {
                        model: "openai/gpt-5.4".into(),
                    },
                },
            },
        };
        config.save(&path).unwrap();
        let loaded = Config::load_single(&path);
        assert_eq!(loaded.timeout, 999);
        assert!((loaded.poll_interval - 1.5).abs() < f64::EPSILON);
        assert_eq!(loaded.default_backend, AgentBackend::Pi);
        assert_eq!(loaded.db.location, DbLocation::ProjectLocal);
        assert_eq!(loaded.backends.claude.model, "sonnet");
        assert_eq!(loaded.backends.pi.model, "openai/gpt-5.4");
        assert_eq!(loaded.policies.len(), 1);
        assert_eq!(loaded.policies[0].pattern, Some("secret".into()));
        assert!(loaded.supervisor.enabled);
        assert_eq!(loaded.supervisor.backends.claude.model, "haiku");
        assert_eq!(loaded.supervisor.backends.pi.model, "openai/gpt-5.4");
        assert_eq!(loaded.supervisor.rules, vec!["be safe"]);
    }

    #[test]
    fn test_supervisor_default() {
        let s = SupervisorConfig::default();
        assert!(!s.enabled);
        assert_eq!(s.backends.claude.model, "");
        assert_eq!(s.backends.pi.model, "");
        assert!(s.rules.is_empty());
    }

    #[test]
    fn test_parse_claude_code_permission_bare_tool() {
        let p = parse_claude_code_permission("Edit", "allow").unwrap();
        assert_eq!(p.tool, "Edit");
        assert_eq!(p.action, "allow");
        assert!(p.pattern.is_none());
    }

    #[test]
    fn test_parse_claude_code_permission_with_pattern() {
        let p = parse_claude_code_permission("Bash(cargo test:*)", "allow").unwrap();
        assert_eq!(p.tool, "Bash");
        assert_eq!(p.action, "allow");
        assert!(p.pattern.is_some());
        let pat = p.pattern.unwrap();
        // The pattern should match "cargo test" followed by anything
        let re = regex::Regex::new(&pat).unwrap();
        assert!(re.is_match("cargo test --release"));
        assert!(re.is_match("cargo test"));
        assert!(!re.is_match("cargo build"));
    }

    #[test]
    fn test_parse_claude_code_permission_deny() {
        let p = parse_claude_code_permission("Bash(rm -rf:*)", "deny").unwrap();
        assert_eq!(p.tool, "Bash");
        assert_eq!(p.action, "deny");
        assert!(p.pattern.is_some());
    }

    #[test]
    fn test_parse_claude_code_permission_path_pattern() {
        // Double-slash prefix is Claude Code's absolute path convention — stripped to single /
        let p = parse_claude_code_permission("Read(//Users/wtj/src/**)", "allow").unwrap();
        assert_eq!(p.tool, "Read");
        assert_eq!(p.action, "allow");
        let pat = p.pattern.unwrap();
        let re = regex::Regex::new(&pat).unwrap();
        // Should match actual file_path values (single slash)
        assert!(re.is_match("/Users/wtj/src/foo/bar.rs"));
        assert!(!re.is_match("/Users/other/file.rs"));
    }

    #[test]
    fn test_parse_claude_code_permission_empty() {
        assert!(parse_claude_code_permission("", "allow").is_none());
    }

    #[test]
    fn test_parse_claude_code_permission_no_tool_name() {
        assert!(parse_claude_code_permission("(pattern)", "allow").is_none());
    }

    #[test]
    fn test_claude_code_pattern_to_regex() {
        // Simple wildcard (anchored with ^)
        assert_eq!(claude_code_pattern_to_regex("foo*"), "(?s)^foo.*");
        // Double wildcard
        assert_eq!(claude_code_pattern_to_regex("foo/**"), "(?s)^foo/.*");
        // Special chars escaped
        assert_eq!(claude_code_pattern_to_regex("a.b"), "(?s)^a\\.b");
        // Colon before wildcard is a prefix separator (dropped)
        assert_eq!(claude_code_pattern_to_regex("cmd:*"), "(?s)^cmd.*");
        // Colon not before wildcard is literal
        assert_eq!(claude_code_pattern_to_regex("a:b"), "(?s)^a:b");
    }

    #[test]
    fn test_parse_claude_code_permission_webfetch_domain() {
        let p = parse_claude_code_permission("WebFetch(domain:example.com)", "allow").unwrap();
        assert_eq!(p.tool, "WebFetch");
        assert_eq!(p.action, "allow");
        assert_eq!(p.pattern, Some("example.com".into()));
        assert_eq!(p.match_mode, MatchMode::Domain);
    }

    #[test]
    fn test_parse_claude_code_permission_webfetch_url_pattern() {
        // Non-domain WebFetch pattern is a regex
        let p = parse_claude_code_permission("WebFetch(https://docs.rs:*)", "allow").unwrap();
        assert_eq!(p.tool, "WebFetch");
        assert_eq!(p.match_mode, MatchMode::Regex);
    }

    #[test]
    fn test_derive_policies_from_settings_files() {
        let dir = tempfile::tempdir().unwrap();

        // Write a settings file with some permissions
        let settings = serde_json::json!({
            "permissions": {
                "allow": ["Edit", "Bash(cargo test:*)"],
                "deny": ["Bash(rm -rf:*)"]
            }
        });
        let settings_path = dir.path().join("settings.json");
        fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

        let policies = derive_policies_from_settings_files(&[settings_path]);
        assert_eq!(policies.len(), 3);
        // First: Edit allow (no pattern)
        assert_eq!(policies[0].tool, "Edit");
        assert_eq!(policies[0].action, "allow");
        assert!(policies[0].pattern.is_none());
        // Second: Bash allow with pattern
        assert_eq!(policies[1].tool, "Bash");
        assert_eq!(policies[1].action, "allow");
        assert!(policies[1].pattern.is_some());
        // Third: Bash deny with pattern
        assert_eq!(policies[2].tool, "Bash");
        assert_eq!(policies[2].action, "deny");
        assert!(policies[2].pattern.is_some());
    }

    #[test]
    fn test_derive_policies_from_missing_files() {
        let policies = derive_policies_from_settings_files(&[
            PathBuf::from("/tmp/nonexistent_cq_test_1.json"),
            PathBuf::from("/tmp/nonexistent_cq_test_2.json"),
        ]);
        assert!(policies.is_empty());
    }

    #[test]
    fn test_derive_policies_file_priority_order() {
        let dir = tempfile::tempdir().unwrap();

        // Higher priority file
        let high = serde_json::json!({
            "permissions": { "allow": ["Write"] }
        });
        let high_path = dir.path().join("high.json");
        fs::write(&high_path, serde_json::to_string(&high).unwrap()).unwrap();

        // Lower priority file
        let low = serde_json::json!({
            "permissions": { "allow": ["Read"] }
        });
        let low_path = dir.path().join("low.json");
        fs::write(&low_path, serde_json::to_string(&low).unwrap()).unwrap();

        let policies = derive_policies_from_settings_files(&[high_path, low_path]);
        assert_eq!(policies.len(), 2);
        // High priority file's policies come first
        assert_eq!(policies[0].tool, "Write");
        assert_eq!(policies[1].tool, "Read");
    }

    #[test]
    fn test_load_project_false_overrides_user_true() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();

        fs::create_dir_all(home.path().join(".cq")).unwrap();
        fs::write(
            home.path().join(".cq").join("config.json"),
            r#"{"supervisor":{"enabled":true,"include_session_context":true},"default_backend":"pi"}"#,
        )
        .unwrap();

        fs::create_dir_all(project.path().join(".cq")).unwrap();
        fs::write(
            project.path().join(".cq").join("config.json"),
            r#"{"supervisor":{"enabled":false,"include_session_context":false},"default_backend":"claude"}"#,
        )
        .unwrap();

        let loaded = Config::load_from_raw_paths(
            &home.path().join(".cq").join("config.json"),
            &project.path().join(".cq").join("config.json"),
            project.path(),
        );
        assert!(!loaded.supervisor.enabled);
        assert!(!loaded.supervisor.include_session_context);
        assert_eq!(loaded.default_backend, AgentBackend::Claude);
    }

    #[test]
    fn test_load_project_explicit_default_value_overrides_user_non_default() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();

        fs::create_dir_all(home.path().join(".cq")).unwrap();
        fs::write(
            home.path().join(".cq").join("config.json"),
            r#"{"timeout":123,"poll_interval":9.5}"#,
        )
        .unwrap();

        fs::create_dir_all(project.path().join(".cq")).unwrap();
        fs::write(
            project.path().join(".cq").join("config.json"),
            format!(
                r#"{{"timeout":{},"poll_interval":{}}}"#,
                default_timeout(),
                default_poll_interval()
            ),
        )
        .unwrap();

        let loaded = Config::load_from_raw_paths(
            &home.path().join(".cq").join("config.json"),
            &project.path().join(".cq").join("config.json"),
            project.path(),
        );
        assert_eq!(loaded.timeout, default_timeout());
        assert!((loaded.poll_interval - default_poll_interval()).abs() < f64::EPSILON);
    }

    #[test]
    fn test_load_project_backend_model_overrides_user_backend_model() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();

        fs::create_dir_all(home.path().join(".cq")).unwrap();
        fs::write(
            home.path().join(".cq").join("config.json"),
            r#"{"backends":{"claude":{"model":"sonnet"},"pi":{"model":"openai/gpt-5.4"}},"supervisor":{"backends":{"claude":{"model":"haiku"}}}}"#,
        )
        .unwrap();

        fs::create_dir_all(project.path().join(".cq")).unwrap();
        fs::write(
            project.path().join(".cq").join("config.json"),
            r#"{"backends":{"claude":{"model":"opus"}},"supervisor":{"backends":{"pi":{"model":"local/qwen"}}}}"#,
        )
        .unwrap();

        let loaded = Config::load_from_raw_paths(
            &home.path().join(".cq").join("config.json"),
            &project.path().join(".cq").join("config.json"),
            project.path(),
        );
        assert_eq!(loaded.backends.claude.model, "opus");
        assert_eq!(loaded.backends.pi.model, "openai/gpt-5.4");
        assert_eq!(loaded.supervisor.backends.claude.model, "haiku");
        assert_eq!(loaded.supervisor.backends.pi.model, "local/qwen");
    }

    #[test]
    fn test_load_project_db_location_overrides_user_db_location() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();

        fs::create_dir_all(home.path().join(".cq")).unwrap();
        fs::write(
            home.path().join(".cq").join("config.json"),
            r#"{"db":{"location":"user"}}"#,
        )
        .unwrap();

        fs::create_dir_all(project.path().join(".cq")).unwrap();
        fs::write(
            project.path().join(".cq").join("config.json"),
            r#"{"db":{"location":"project_local"}}"#,
        )
        .unwrap();

        let loaded = Config::load_from_raw_paths(
            &home.path().join(".cq").join("config.json"),
            &project.path().join(".cq").join("config.json"),
            project.path(),
        );
        assert_eq!(loaded.db.location, DbLocation::ProjectLocal);
    }

    #[test]
    fn test_db_path_for_uses_project_local_db_when_configured() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();

        fs::create_dir_all(home.path().join(".cq")).unwrap();
        fs::write(
            home.path().join(".cq").join("config.json"),
            r#"{"db":{"location":"user"}}"#,
        )
        .unwrap();

        fs::create_dir_all(project.path().join(".cq")).unwrap();
        fs::write(
            project.path().join(".cq").join("config.json"),
            r#"{"db":{"location":"project_local"}}"#,
        )
        .unwrap();

        let path = db_path_for(project.path());
        assert_eq!(path, project.path().join(".cq").join("cq.db"));
    }
}
