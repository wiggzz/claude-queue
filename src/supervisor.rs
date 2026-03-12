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
- Base64-encoded content, eval, or indirect execution should be DENIED unless clearly benign.
- Read-only operations (reading files, searching, listing) are generally safe to APPROVE.
- Writing or editing files within the project directory is generally safe to APPROVE.
- Network requests, package installs, and system modifications deserve scrutiny.
";

#[derive(Debug)]
pub enum Decision {
    Approve(String),
    Deny(String),
    Escalate(String),
}

#[derive(Deserialize)]
struct LlmResponse {
    decision: String,
    reason: String,
}

fn build_prompt(rules: &[String], tool_name: &str, tool_input: &str) -> String {
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
        "\nRespond with JSON only: {\"decision\": \"approve|deny|escalate\", \"reason\": \"brief explanation\"}"
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
            "--output-format", "json",
            "--model", &config.model,
            "--max-turns", "1",
            &prompt,
        ])
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            return Ok(Decision::Escalate(format!("Failed to invoke supervisor: {e}")));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Ok(Decision::Escalate(format!(
            "Supervisor process failed: {stderr}"
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // The claude CLI with --output-format json wraps the result; extract the text content
    let response_text = extract_text_from_claude_output(&stdout);

    let parsed: LlmResponse = match serde_json::from_str(&response_text) {
        Ok(r) => r,
        Err(_) => {
            return Ok(Decision::Escalate(format!(
                "Failed to parse supervisor response: {response_text}"
            )));
        }
    };

    match parsed.decision.to_lowercase().as_str() {
        "approve" => Ok(Decision::Approve(parsed.reason)),
        "deny" => Ok(Decision::Deny(parsed.reason)),
        _ => Ok(Decision::Escalate(parsed.reason)),
    }
}

/// Extract the text content from claude CLI JSON output.
/// The claude CLI --output-format json returns a structure with a "result" field
/// containing the assistant's text response.
fn extract_text_from_claude_output(output: &str) -> String {
    // Try to parse as claude CLI JSON output format
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(output) {
        // Claude CLI format: {"result": "...text..."}
        if let Some(result) = val.get("result").and_then(|v| v.as_str()) {
            return result.trim().to_string();
        }
        // Alternative: array of content blocks
        if let Some(result) = val.get("result").and_then(|v| v.as_array()) {
            for block in result {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        return text.trim().to_string();
                    }
                }
            }
        }
    }
    // Fallback: use raw output
    output.trim().to_string()
}
