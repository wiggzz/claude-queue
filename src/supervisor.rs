use crate::config::SupervisorConfig;
use serde::Deserialize;
use std::process::Command;

const SYSTEM_PROMPT: &str = "\
You are a security-conscious supervisor reviewing tool calls from an automated coding agent.

Default guidelines:
- Ambiguous, obfuscated, or difficult to understand commands should be ESCALATED.
- If a tool call's purpose doesn't clearly relate to the agent's task, ESCALATE.
- If you can't determine what files or systems would be affected, ESCALATE.
- Piped commands with many stages deserve extra scrutiny.
- Base64-encoded content, eval, or indirect execution should be ESCALATED.
- Read-only operations (reading files, searching, listing) are generally safe to APPROVE.
- Writing or editing files within the project directory is generally safe to APPROVE.
- Network requests, package installs, and system modifications should be ESCALATED.
- You can only APPROVE or ESCALATE. You cannot deny — only a human operator can deny.
";

#[derive(Debug)]
pub enum Decision {
    Approve(String),
    Escalate {
        reason: String,
        summary: Option<String>,
    },
}

#[derive(Deserialize)]
struct LlmResponse {
    decision: String,
    reason: String,
    summary: Option<String>,
}

pub(crate) fn build_prompt(rules: &[String], tool_name: &str, tool_input: &str) -> String {
    let mut prompt = String::from(SYSTEM_PROMPT);

    // Add user-defined rules
    if !rules.is_empty() {
        prompt.push_str("\nAdditional rules from the operator:\n");
        for rule in rules {
            prompt.push_str(&format!("- {rule}\n"));
        }
    }

    // Add session context from env vars
    let session_name = std::env::var("CQ_SESSION_NAME").unwrap_or_default();
    let session_prompt = std::env::var("CQ_SESSION_PROMPT").unwrap_or_default();
    let session_cwd = std::env::var("CQ_SESSION_CWD").unwrap_or_default();

    if !session_name.is_empty() || !session_prompt.is_empty() || !session_cwd.is_empty() {
        prompt.push_str("\nSession context:\n");
        if !session_name.is_empty() {
            prompt.push_str(&format!("- Session name: {session_name}\n"));
        }
        if !session_prompt.is_empty() {
            prompt.push_str(&format!("- Session task: {session_prompt}\n"));
        }
        if !session_cwd.is_empty() {
            prompt.push_str(&format!("- Working directory: {session_cwd}\n"));
        }
    }

    // Add tool call details
    prompt.push_str(&format!(
        "\nTool call to review:\n- Tool: {tool_name}\n- Input: {tool_input}\n"
    ));

    prompt.push_str(
        "\nRespond with JSON only: {\"decision\": \"approve|escalate\", \"reason\": \"brief explanation\"}\n\
        If you choose \"escalate\", also include a \"summary\" field: a single plain-English sentence \
        describing what the tool call does from a neutral perspective (e.g. \"Pushes current branch to origin/main\"). \
        This summary will be shown to the human operator for approval.\n\
        You cannot deny — only approve or escalate. The human operator makes all deny decisions."
    );

    prompt
}

pub fn evaluate(
    config: &SupervisorConfig,
    tool_name: &str,
    tool_input: &str,
) -> Result<Decision, Box<dyn std::error::Error>> {
    let prompt = build_prompt(&config.rules, tool_name, tool_input);

    let output = Command::new("claude")
        .args([
            "-p",
            "--output-format",
            "json",
            "--model",
            &config.model,
            "--max-turns",
            "1",
            "--no-session-persistence",
            &prompt,
        ])
        .env_remove("CLAUDECODE")
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            return Ok(Decision::Escalate {
                reason: format!("Failed to invoke supervisor: {e}"),
                summary: None,
            });
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or("signal".into());
        return Ok(Decision::Escalate {
            reason: format!(
                "Supervisor process failed (exit {code}): stderr={stderr} stdout={}",
                &stdout[..stdout.len().min(1000)]
            ),
            summary: None,
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // The claude CLI with --output-format json wraps the result; extract the text content
    let response_text = extract_text_from_claude_output(&stdout);

    let parsed: LlmResponse = match serde_json::from_str(&response_text) {
        Ok(r) => r,
        Err(_) => {
            return Ok(Decision::Escalate {
                reason: format!("Failed to parse supervisor response: {response_text}"),
                summary: None,
            });
        }
    };

    match parsed.decision.to_lowercase().as_str() {
        "approve" => Ok(Decision::Approve(parsed.reason)),
        // Supervisor cannot deny — only approve or escalate to human.
        // This ensures the orchestrator always has visibility into blocked calls.
        _ => Ok(Decision::Escalate {
            reason: parsed.reason,
            summary: parsed.summary,
        }),
    }
}

/// Extract the text content from claude CLI JSON output.
/// The claude CLI --output-format json returns a structure with a "result" field
/// containing the assistant's text response.
pub(crate) fn extract_text_from_claude_output(output: &str) -> String {
    // Try to parse as claude CLI JSON output format
    // Try to parse as claude CLI JSON output format
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(output) {
        // Claude CLI format: {"result": "...text..."}
        if let Some(result) = val.get("result").and_then(|v| v.as_str()) {
            return strip_markdown_fencing(result.trim());
        }
        // Alternative: array of content blocks
        if let Some(result) = val.get("result").and_then(|v| v.as_array()) {
            for block in result {
                if block.get("type").and_then(|t| t.as_str()) == Some("text")
                    && let Some(text) = block.get("text").and_then(|t| t.as_str())
                {
                    return strip_markdown_fencing(text.trim());
                }
            }
        }
    }
    // Fallback: use raw output
    strip_markdown_fencing(output.trim())
}

/// Strip markdown code fencing (```json ... ```) from LLM responses.
pub(crate) fn strip_markdown_fencing(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.starts_with("```") {
        // Remove opening fence (```json or ```)
        let after_open = if let Some(newline_pos) = trimmed.find('\n') {
            &trimmed[newline_pos + 1..]
        } else {
            return trimmed.to_string();
        };
        // Remove closing fence
        let content = if after_open.trim_end().ends_with("```") {
            let end = after_open.rfind("```").unwrap_or(after_open.len());
            &after_open[..end]
        } else {
            after_open
        };
        content.trim().to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_prompt_includes_system() {
        let prompt = build_prompt(&[], "Bash", "ls");
        assert!(
            prompt.contains("security-conscious supervisor"),
            "prompt should contain SYSTEM_PROMPT text"
        );
    }

    #[test]
    fn test_build_prompt_with_rules() {
        let rules = vec!["Never allow rm -rf".to_string()];
        let prompt = build_prompt(&rules, "Bash", "ls");
        assert!(
            prompt.contains("Additional rules"),
            "prompt should contain Additional rules section"
        );
        assert!(
            prompt.contains("Never allow rm -rf"),
            "prompt should contain the rule text"
        );
    }

    #[test]
    fn test_build_prompt_tool_details() {
        let prompt = build_prompt(&[], "Write", "/tmp/foo.txt");
        assert!(
            prompt.contains("Tool: Write"),
            "prompt should contain tool name"
        );
        assert!(
            prompt.contains("Input: /tmp/foo.txt"),
            "prompt should contain tool input"
        );
    }

    #[test]
    fn test_strip_markdown_no_fencing() {
        assert_eq!(strip_markdown_fencing("plain text"), "plain text");
    }

    #[test]
    fn test_strip_markdown_json_fencing() {
        let input = "```json\n{\"key\": \"value\"}\n```";
        assert_eq!(strip_markdown_fencing(input), "{\"key\": \"value\"}");
    }

    #[test]
    fn test_strip_markdown_generic_fencing() {
        let input = "```\nhello world\n```";
        assert_eq!(strip_markdown_fencing(input), "hello world");
    }

    #[test]
    fn test_extract_text_result_string() {
        let input = r#"{"result": "text"}"#;
        assert_eq!(extract_text_from_claude_output(input), "text");
    }

    #[test]
    fn test_extract_text_result_array() {
        let input = r#"{"result": [{"type":"text","text":"hello"}]}"#;
        assert_eq!(extract_text_from_claude_output(input), "hello");
    }

    #[test]
    fn test_extract_text_raw_fallback() {
        let input = "not json at all";
        assert_eq!(extract_text_from_claude_output(input), "not json at all");
    }
}
