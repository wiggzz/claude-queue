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
pub struct Config {
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default = "default_poll_interval")]
    pub poll_interval: f64,
    #[serde(default)]
    pub policies: Vec<Policy>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            timeout: default_timeout(),
            poll_interval: default_poll_interval(),
            policies: Vec::new(),
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

        Config {
            timeout,
            poll_interval,
            policies,
        }
    }

    pub fn load_user_only() -> Self {
        Self::load_single(&user_config_path())
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
        };
        let _ = config.save(&path);
    }
}
