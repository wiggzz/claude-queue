/// For Write/Edit tool calls, extract file_path and show it prominently.
/// Returns a formatted string like "[path/to/file.rs] {truncated...}" for file tools,
/// or a plain truncated input for other tools.
pub fn format_tool_input(tool_name: &str, tool_input: &str, max_len: usize) -> String {
    // Bash tool: extract and display the command string prominently
    if tool_name == "Bash"
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(tool_input)
        && let Some(cmd) = v.get("command").and_then(|c| c.as_str())
    {
        let prefix = "$ ";
        let remaining = max_len.saturating_sub(prefix.len());
        if cmd.len() > remaining && remaining > 3 {
            return format!("{prefix}{}...", &cmd[..remaining - 3]);
        }
        if cmd.len() > remaining {
            return format!("{prefix}{}", &cmd[..remaining]);
        }
        return format!("{prefix}{cmd}");
    }
    if matches!(tool_name, "Write" | "Edit")
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(tool_input)
        && let Some(fp) = v.get("file_path").and_then(|f| f.as_str())
    {
        let short_path = if fp.len() > 40 {
            format!("...{}", &fp[fp.len() - 37..])
        } else {
            fp.to_string()
        };
        let prefix = format!("[{}]", short_path);
        let remaining = max_len.saturating_sub(prefix.len() + 1);
        if remaining > 3 {
            let content_key = if tool_name == "Write" {
                "content"
            } else {
                "new_string"
            };
            if let Some(content) = v.get(content_key).and_then(|c| c.as_str()) {
                let snippet: String = content
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(remaining - 3)
                    .collect();
                return format!("{prefix} {snippet}...");
            }
        }
        return prefix;
    }
    // Fallback: plain truncation
    if tool_input.len() > max_len {
        format!("{}...", &tool_input[..max_len - 3])
    } else {
        tool_input.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_tool_shows_filepath() {
        let input = r#"{"file_path":"src/main.rs","content":"fn main() {}"}"#;
        let result = format_tool_input("Write", input, 80);
        assert!(result.starts_with("[src/main.rs]"), "got: {result}");
    }

    #[test]
    fn test_edit_tool_shows_filepath() {
        let input = r#"{"file_path":"src/lib.rs","new_string":"let x = 1;"}"#;
        let result = format_tool_input("Edit", input, 80);
        assert!(result.starts_with("[src/lib.rs]"), "got: {result}");
    }

    #[test]
    fn test_long_path_truncated() {
        let long_path = "a/".repeat(25) + "file.rs"; // well over 40 chars
        let input = format!(r#"{{"file_path":"{}","content":"hello"}}"#, long_path);
        let result = format_tool_input("Write", &input, 80);
        assert!(
            result.starts_with("[..."),
            "expected truncated path, got: {result}"
        );
    }

    #[test]
    fn test_non_file_tool_plain_truncation() {
        let input = "a]".repeat(50);
        let result = format_tool_input("Bash", &input, 20);
        assert_eq!(result.len(), 20);
        assert!(result.ends_with("..."), "got: {result}");
    }

    #[test]
    fn test_short_input_no_truncation() {
        let input = "echo hi";
        let result = format_tool_input("Bash", input, 80);
        assert_eq!(result, "echo hi");
    }

    #[test]
    fn test_bash_tool_shows_command() {
        let input = r#"{"command":"git status --short","description":"check status"}"#;
        let result = format_tool_input("Bash", input, 80);
        assert_eq!(result, "$ git status --short", "got: {result}");
    }

    #[test]
    fn test_bash_tool_long_command_truncated() {
        let long_cmd = "echo ".to_string() + &"x".repeat(100);
        let input = format!(r#"{{"command":"{}"}}"#, long_cmd);
        let result = format_tool_input("Bash", &input, 40);
        assert!(result.starts_with("$ echo "), "got: {result}");
        assert!(result.ends_with("..."), "got: {result}");
        assert!(result.len() <= 40, "got len: {}", result.len());
    }

    #[test]
    fn test_invalid_json_fallback() {
        let input = "not json at all, just some long text that should be truncated";
        let result = format_tool_input("Write", input, 30);
        assert_eq!(result.len(), 30);
        assert!(result.ends_with("..."), "got: {result}");
    }

    #[test]
    fn test_missing_filepath_fallback() {
        let input = r#"{"content":"hello world"}"#;
        let result = format_tool_input("Write", input, 80);
        // No file_path => falls back to plain, input is short enough
        assert_eq!(result, input);
    }
}
