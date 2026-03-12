/// For Write/Edit tool calls, extract file_path and show it prominently.
/// Returns a formatted string like "[path/to/file.rs] {truncated...}" for file tools,
/// or a plain truncated input for other tools.
pub fn format_tool_input(tool_name: &str, tool_input: &str, max_len: usize) -> String {
    if matches!(tool_name, "Write" | "Edit") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(tool_input) {
            if let Some(fp) = v.get("file_path").and_then(|f| f.as_str()) {
                let short_path = if fp.len() > 40 {
                    format!("...{}", &fp[fp.len() - 37..])
                } else {
                    fp.to_string()
                };
                let prefix = format!("[{}]", short_path);
                let remaining = max_len.saturating_sub(prefix.len() + 1);
                if remaining > 3 {
                    let content_key = if tool_name == "Write" { "content" } else { "new_string" };
                    if let Some(content) = v.get(content_key).and_then(|c| c.as_str()) {
                        let snippet: String = content.chars()
                            .filter(|c| !c.is_control())
                            .take(remaining - 3)
                            .collect();
                        return format!("{prefix} {snippet}...");
                    }
                }
                return prefix;
            }
        }
    }
    // Fallback: plain truncation
    if tool_input.len() > max_len {
        format!("{}...", &tool_input[..max_len - 3])
    } else {
        tool_input.to_string()
    }
}
