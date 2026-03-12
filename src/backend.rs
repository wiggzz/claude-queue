use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum, Default)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "kebab-case")]
pub enum AgentBackend {
    #[default]
    Claude,
    Codex,
}

impl AgentBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentBackend::Claude => "claude",
            AgentBackend::Codex => "codex",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "codex" => AgentBackend::Codex,
            _ => AgentBackend::Claude,
        }
    }

    pub fn extract_output(self, raw: &str) -> String {
        match self {
            AgentBackend::Claude => raw.to_string(),
            AgentBackend::Codex => extract_codex_output(raw),
        }
    }

    pub fn extract_external_session_id(self, raw: &str) -> Option<String> {
        match self {
            AgentBackend::Claude => None,
            AgentBackend::Codex => extract_codex_thread_id(raw),
        }
    }
}

fn extract_codex_output(raw: &str) -> String {
    let mut messages = Vec::new();

    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        let Some(item) = val.get("item") else {
            continue;
        };

        if item.get("type").and_then(|v| v.as_str()) == Some("agent_message")
            && let Some(text) = item.get("text").and_then(|v| v.as_str())
        {
            messages.push(text.trim().to_string());
        }
    }

    if messages.is_empty() {
        raw.to_string()
    } else {
        messages.join("\n\n")
    }
}

fn extract_codex_thread_id(raw: &str) -> Option<String> {
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        if val.get("type").and_then(|v| v.as_str()) == Some("thread.started")
            && let Some(thread_id) = val.get("thread_id").and_then(|v| v.as_str())
        {
            return Some(thread_id.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_codex_output_collects_agent_messages() {
        let raw = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t1\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"first\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"second\"}}\n"
        );

        assert_eq!(AgentBackend::Codex.extract_output(raw), "first\n\nsecond");
    }

    #[test]
    fn test_extract_codex_output_falls_back_to_raw() {
        let raw = "{\"type\":\"thread.started\",\"thread_id\":\"t1\"}\n";
        assert_eq!(AgentBackend::Codex.extract_output(raw), raw);
    }

    #[test]
    fn test_extract_codex_thread_id() {
        let raw = concat!(
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-123\"}\n"
        );

        assert_eq!(
            AgentBackend::Codex.extract_external_session_id(raw),
            Some("thread-123".into())
        );
    }
}
