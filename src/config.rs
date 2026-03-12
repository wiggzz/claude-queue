use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub tool: String,
    pub action: String, // "allow", "deny", "ask"
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>, // regex pattern to match against tool_input (e.g. Bash command)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_supervisor_model")]
    pub model: String,
    #[serde(default)]
    pub rules: Vec<String>,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        SupervisorConfig {
            enabled: false,
            model: default_supervisor_model(),
            rules: Vec::new(),
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

/// Create default user config if it doesn't exist.
pub fn ensure_user_config() {
    let path = user_config_path();
    if !path.exists() {
        let config = Config {
            timeout: default_timeout(),
            poll_interval: default_poll_interval(),
            policies: vec![
                Policy { tool: "Read".into(), action: "allow".into(), pattern: None },
                Policy { tool: "Glob".into(), action: "allow".into(), pattern: None },
                Policy { tool: "Grep".into(), action: "allow".into(), pattern: None },
                Policy { tool: "LSP".into(), action: "allow".into(), pattern: None },
            ],
            supervisor: SupervisorConfig::default(),
        };
        let _ = config.save(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use std::io::Write;

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
            policies: vec![Policy { tool: "Write".into(), action: "ask".into(), pattern: Some("secret".into()) }],
            supervisor: SupervisorConfig { enabled: true, model: "sonnet".into(), rules: vec!["be safe".into()] },
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
}
