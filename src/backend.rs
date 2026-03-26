use clap::ValueEnum;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum, Default)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "kebab-case")]
pub enum AgentBackend {
    #[default]
    Claude,
    Pi,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalToolCall {
    pub tool_name: String,
    pub tool_input: String,
}

impl AgentBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentBackend::Claude => "claude",
            AgentBackend::Pi => "pi",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "pi" => AgentBackend::Pi,
            _ => AgentBackend::Claude,
        }
    }

    pub fn parse_env(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(AgentBackend::Claude),
            "pi" => Some(AgentBackend::Pi),
            _ => None,
        }
    }

    pub fn canonicalize_tool_call(
        self,
        tool_name: &str,
        tool_input: Value,
    ) -> Result<CanonicalToolCall, serde_json::Error> {
        let (tool_name, tool_input) = match self {
            AgentBackend::Claude => (tool_name.to_string(), tool_input),
            AgentBackend::Pi => canonicalize_pi_tool_call(tool_name, tool_input),
        };

        Ok(CanonicalToolCall {
            tool_name,
            tool_input: serde_json::to_string(&tool_input)?,
        })
    }

    pub fn extract_output(self, raw: &str) -> String {
        sanitize_terminal_output(raw).trim().to_string()
    }
}

fn canonicalize_pi_tool_call(tool_name: &str, tool_input: Value) -> (String, Value) {
    let canonical_name = match tool_name {
        "bash" => "Bash",
        "read" => "Read",
        "edit" => "Edit",
        "write" => "Write",
        "grep" => "Grep",
        "find" => "Glob",
        "ls" => "LS",
        _ => tool_name,
    }
    .to_string();

    let mut input = match tool_input {
        Value::Object(map) => map,
        other => return (canonical_name, other),
    };

    match tool_name {
        "read" | "edit" | "write" => rename_key(&mut input, "path", "file_path"),
        _ => {}
    }
    if tool_name == "edit" {
        rename_key(&mut input, "oldText", "old_string");
        rename_key(&mut input, "newText", "new_string");
    }

    (canonical_name, Value::Object(input))
}

fn rename_key(map: &mut Map<String, Value>, from: &str, to: &str) {
    if map.contains_key(to) {
        return;
    }
    if let Some(value) = map.remove(from) {
        map.insert(to.to_string(), value);
    }
}

pub(crate) fn sanitize_terminal_output(raw: &str) -> String {
    let without_ansi = strip_ansi_sequences(raw);
    let without_backspaces = apply_backspaces(&without_ansi);
    without_backspaces
        .replace('\r', "")
        .chars()
        .filter(|ch| matches!(ch, '\n' | '\t') || !ch.is_control())
        .collect()
}

fn strip_ansi_sequences(raw: &str) -> String {
    let csi = Regex::new(r"\x1B\[[0-?]*[ -/]*[@-~]").unwrap();
    let osc = Regex::new(r"\x1B\][^\x07\x1B]*(?:\x07|\x1B\\)").unwrap();
    let without_osc = osc.replace_all(raw, "");
    csi.replace_all(&without_osc, "").into_owned()
}

fn apply_backspaces(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch == '\u{8}' {
            out.pop();
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_claude_tool_call_is_passthrough() {
        let canonical = AgentBackend::Claude
            .canonicalize_tool_call("Bash", json!({"command": "pwd"}))
            .unwrap();

        assert_eq!(canonical.tool_name, "Bash");
        assert_eq!(canonical.tool_input, r#"{"command":"pwd"}"#);
    }

    #[test]
    fn test_pi_edit_tool_call_maps_to_canonical_shape() {
        let canonical = AgentBackend::Pi
            .canonicalize_tool_call(
                "edit",
                json!({
                    "path": "src/main.rs",
                    "oldText": "old",
                    "newText": "new"
                }),
            )
            .unwrap();

        assert_eq!(canonical.tool_name, "Edit");
        let input: serde_json::Value = serde_json::from_str(&canonical.tool_input).unwrap();
        assert_eq!(
            input,
            json!({
                "file_path": "src/main.rs",
                "old_string": "old",
                "new_string": "new"
            })
        );
    }

    #[test]
    fn test_pi_find_maps_to_glob() {
        let canonical = AgentBackend::Pi
            .canonicalize_tool_call("find", json!({"path": ".", "pattern": "*.rs"}))
            .unwrap();

        assert_eq!(canonical.tool_name, "Glob");
        assert_eq!(canonical.tool_input, r#"{"path":".","pattern":"*.rs"}"#);
    }

    #[test]
    fn test_extract_output_strips_terminal_noise() {
        let raw = "^\u{8}D\u{8}\u{8}Hi!\r\n\u{1b}[?25h\u{1b}]9;4;0;\u{7}";
        assert_eq!(AgentBackend::Claude.extract_output(raw), "Hi!");
    }

    #[test]
    fn test_extract_output_removes_remaining_control_characters() {
        let raw = "ok\u{7}\u{b}\u{c}\u{1b}[2Jdone\u{7}";
        assert_eq!(AgentBackend::Claude.extract_output(raw), "okdone");
    }
}
