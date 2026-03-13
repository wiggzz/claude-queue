use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_supervisor_model")]
    pub model: String,
    #[serde(default)]
    pub rules: Vec<String>,
    /// Whether to include the agent's session prompt/task in the supervisor context.
    /// Default: false. When false, the supervisor evaluates tool calls purely on their
    /// own merit, preventing the agent's prompt from biasing approval decisions.
    #[serde(default)]
    pub include_session_context: bool,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        SupervisorConfig {
            enabled: false,
            model: default_supervisor_model(),
            rules: Vec::new(),
            include_session_context: false,
        }
    }
}

fn default_supervisor_model() -> String {
    "haiku".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default = "default_poll_interval")]
    pub poll_interval: f64,
    #[serde(default)]
    pub policies: Vec<Policy>,
    #[serde(default)]
    pub supervisor: SupervisorConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            timeout: default_timeout(),
            poll_interval: default_poll_interval(),
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
        let user_config = Self::load_single(&user_config_path());
        let project_config = Self::load_single(&project_config_path(project_dir));

        let timeout = if project_config.timeout != default_timeout() {
            project_config.timeout
        } else {
            user_config.timeout
        };

        let poll_interval = if project_config.poll_interval != default_poll_interval() {
            project_config.poll_interval
        } else {
            user_config.poll_interval
        };

        // Project policies first (higher priority), then user policies
        let mut policies = project_config.policies;
        policies.extend(user_config.policies);

        // Append Claude Code permission-derived policies as lowest-priority fallback.
        // This makes cq work OOTB without separate config — if Claude Code trusts a tool,
        // cq will too.
        let cc_policies = derive_claude_code_policies(project_dir);
        policies.extend(cc_policies);

        // Supervisor: project config wins for enabled/model, rules are merged (project first)
        let supervisor = SupervisorConfig {
            enabled: project_config.supervisor.enabled || user_config.supervisor.enabled,
            model: if !project_config.supervisor.model.is_empty()
                && project_config.supervisor.model != default_supervisor_model()
            {
                project_config.supervisor.model
            } else {
                user_config.supervisor.model
            },
            rules: {
                let mut rules = project_config.supervisor.rules;
                rules.extend(user_config.supervisor.rules);
                rules
            },
            include_session_context: project_config.supervisor.include_session_context
                || user_config.supervisor.include_session_context,
        };

        Config {
            timeout,
            poll_interval,
            policies,
            supervisor,
        }
    }

    fn load_single(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => Config::default(),
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
}

/// Load a single config file (for editing project or user config directly).
pub fn load_file(path: &Path) -> Config {
    Config::load_single(path)
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
    dirs_home().join(".cq").join("cq.db")
}

pub fn log_dir() -> PathBuf {
    dirs_home().join(".cq").join("logs")
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
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
                model: default_supervisor_model(),
                rules: Vec::new(),
                include_session_context: false,
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
        assert!(c.policies.is_empty());
        assert!(!c.supervisor.enabled);
        assert_eq!(c.supervisor.model, "haiku");
    }

    #[test]
    fn test_load_missing_file() {
        let c = Config::load_single(Path::new("/tmp/nonexistent_cq_config_test.json"));
        assert_eq!(c.timeout, 86400);
        assert!((c.poll_interval - 0.5).abs() < f64::EPSILON);
        assert!(c.policies.is_empty());
    }

    #[test]
    fn test_load_valid_config() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, r#"{{"timeout": 300, "poll_interval": 2.0, "policies": [{{"tool": "Bash", "action": "deny"}}], "supervisor": {{"enabled": true, "model": "opus", "rules": ["no secrets"]}}}}"#).unwrap();
        let c = Config::load_single(f.path());
        assert_eq!(c.timeout, 300);
        assert!((c.poll_interval - 2.0).abs() < f64::EPSILON);
        assert_eq!(c.policies.len(), 1);
        assert_eq!(c.policies[0].tool, "Bash");
        assert_eq!(c.policies[0].action, "deny");
        assert!(c.supervisor.enabled);
        assert_eq!(c.supervisor.model, "opus");
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
            policies: vec![Policy {
                tool: "Write".into(),
                action: "ask".into(),
                pattern: Some("secret".into()),
                match_mode: MatchMode::default(),
            }],
            supervisor: SupervisorConfig {
                enabled: true,
                model: "sonnet".into(),
                rules: vec!["be safe".into()],
                include_session_context: false,
            },
        };
        config.save(&path).unwrap();
        let loaded = Config::load_single(&path);
        assert_eq!(loaded.timeout, 999);
        assert!((loaded.poll_interval - 1.5).abs() < f64::EPSILON);
        assert_eq!(loaded.policies.len(), 1);
        assert_eq!(loaded.policies[0].pattern, Some("secret".into()));
        assert!(loaded.supervisor.enabled);
        assert_eq!(loaded.supervisor.model, "sonnet");
        assert_eq!(loaded.supervisor.rules, vec!["be safe"]);
    }

    #[test]
    fn test_supervisor_default() {
        let s = SupervisorConfig::default();
        assert!(!s.enabled);
        assert_eq!(s.model, "haiku");
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
}
