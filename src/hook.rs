use crate::audit;
use crate::backend::{AgentBackend, CanonicalToolCall};
use crate::config::{self, Config};
use crate::db::Db;
use crate::policy;
use crate::supervisor;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
struct ClaudeHookInput {
    session_id: Option<String>,
    tool_name: Option<String>,
    tool_input: Option<Value>,
    #[allow(dead_code)]
    #[serde(flatten)]
    _extra: serde_json::Map<String, Value>,
}

#[derive(Deserialize)]
struct PiHookInput {
    #[serde(rename = "toolName")]
    tool_name: String,
    input: Value,
    #[allow(dead_code)]
    #[serde(rename = "toolCallId")]
    tool_call_id: Option<String>,
}

#[derive(Serialize)]
struct HookOutput {
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: HookDecision,
}

#[derive(Serialize)]
struct HookDecision {
    #[serde(rename = "hookEventName")]
    hook_event_name: String,
    #[serde(rename = "permissionDecision")]
    permission_decision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "permissionDecisionReason")]
    permission_decision_reason: Option<String>,
}

#[derive(Serialize)]
struct PiHookOutput {
    decision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

enum Decision {
    Allow,
    Deny(Option<String>),
}

impl HookOutput {
    fn allow() -> Self {
        HookOutput {
            hook_specific_output: HookDecision {
                hook_event_name: "PreToolUse".into(),
                permission_decision: "allow".into(),
                permission_decision_reason: None,
            },
        }
    }

    fn deny(reason: Option<String>) -> Self {
        HookOutput {
            hook_specific_output: HookDecision {
                hook_event_name: "PreToolUse".into(),
                permission_decision: "deny".into(),
                permission_decision_reason: reason,
            },
        }
    }
}

pub fn run(agent: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    match agent.unwrap_or("claude") {
        "claude" => run_claude(),
        "pi" => run_pi(),
        other => Err(format!("Unknown hook backend '{other}'").into()),
    }
}

fn run_claude() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("CQ_MANAGED").is_err() {
        print_and_exit(HookOutput::allow());
    }

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let hook_input: ClaudeHookInput = serde_json::from_str(&input)?;

    let session_id = managed_session_id(hook_input.session_id);
    let tool_name = hook_input.tool_name.unwrap_or_default();
    let tool_input = hook_input.tool_input.unwrap_or(Value::Null);
    let canonical = AgentBackend::Claude.canonicalize_tool_call(&tool_name, tool_input)?;

    match evaluate_tool_call(&session_id, &canonical)? {
        Decision::Allow => print_and_exit(HookOutput::allow()),
        Decision::Deny(reason) => print_and_exit(HookOutput::deny(reason)),
    }
}

fn run_pi() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("CQ_MANAGED").is_err() {
        println!("{}", serde_json::to_string(&PiHookOutput::allow())?);
        return Ok(());
    }

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let hook_input: PiHookInput = serde_json::from_str(&input)?;

    let session_id = managed_session_id(None);
    let canonical =
        AgentBackend::Pi.canonicalize_tool_call(&hook_input.tool_name, hook_input.input)?;

    let output = match evaluate_tool_call(&session_id, &canonical)? {
        Decision::Allow => PiHookOutput::allow(),
        Decision::Deny(reason) => PiHookOutput::deny(reason),
    };

    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}

impl PiHookOutput {
    fn allow() -> Self {
        PiHookOutput {
            decision: "allow".into(),
            reason: None,
        }
    }

    fn deny(reason: Option<String>) -> Self {
        PiHookOutput {
            decision: "deny".into(),
            reason,
        }
    }
}

fn evaluate_tool_call(
    session_id: &str,
    tool_call: &CanonicalToolCall,
) -> Result<Decision, Box<dyn std::error::Error>> {
    let config = load_config();

    if let Some(decision) = policy::check(
        &tool_call.tool_name,
        &tool_call.tool_input,
        &config.policies,
    ) {
        match decision.as_str() {
            "allow" => {
                audit::log(
                    session_id,
                    &tool_call.tool_name,
                    &tool_call.tool_input,
                    "approve",
                    "Allowed by policy",
                    "policy",
                );
                return Ok(Decision::Allow);
            }
            "deny" => {
                let reason = format!("Denied by policy for tool: {}", tool_call.tool_name);
                audit::log(
                    session_id,
                    &tool_call.tool_name,
                    &tool_call.tool_input,
                    "deny",
                    &reason,
                    "policy",
                );
                return Ok(Decision::Deny(Some(reason)));
            }
            _ => {}
        }
    }

    let mut escalation_summary: Option<String> = None;
    if config.supervisor.enabled {
        match supervisor::evaluate(
            &config.supervisor,
            &tool_call.tool_name,
            &tool_call.tool_input,
        ) {
            Ok(supervisor::Decision::Approve(reason)) => {
                eprintln!("[cq supervisor] approved: {reason}");
                audit::log(
                    session_id,
                    &tool_call.tool_name,
                    &tool_call.tool_input,
                    "approve",
                    &reason,
                    "supervisor",
                );
                return Ok(Decision::Allow);
            }
            Ok(supervisor::Decision::Escalate { reason, summary }) => {
                eprintln!("[cq supervisor] escalated: {reason}");
                if let Some(ref summary_text) = summary {
                    eprintln!("[cq supervisor] summary: {summary_text}");
                }
                audit::log(
                    session_id,
                    &tool_call.tool_name,
                    &tool_call.tool_input,
                    "escalate",
                    &reason,
                    "supervisor",
                );
                escalation_summary = summary;
            }
            Err(e) => {
                eprintln!("[cq supervisor] error, escalating: {e}");
            }
        }
    }

    let db_path = config::db_path();
    let db = Db::open(&db_path)?;
    let tc_id = db.insert_tool_call_with_summary(
        session_id,
        &tool_call.tool_name,
        &tool_call.tool_input,
        escalation_summary.as_deref(),
    )?;

    let timeout = Duration::from_secs(config.timeout);
    let poll = Duration::from_secs_f64(config.poll_interval);
    let start = Instant::now();

    loop {
        std::thread::sleep(poll);

        if start.elapsed() > timeout {
            db.resolve_tool_call(tc_id, "timed_out", Some("Approval timeout"))?;
            return Ok(Decision::Deny(Some("Approval timed out".into())));
        }

        if let Some((status, reason)) = db.get_tool_call_status(tc_id)? {
            match status.as_str() {
                "approved" => return Ok(Decision::Allow),
                "denied" => return Ok(Decision::Deny(reason)),
                "timed_out" => return Ok(Decision::Deny(Some("Timed out".into()))),
                "pending" => continue,
                _ => continue,
            }
        }
    }
}

fn managed_session_id(hook_session_id: Option<String>) -> String {
    std::env::var("CQ_SESSION_ID")
        .ok()
        .or(hook_session_id)
        .unwrap_or_else(|| "unknown".into())
}

fn load_config() -> Config {
    let project_dir = std::env::var("CQ_PROJECT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    Config::load(&project_dir)
}

fn print_and_exit(output: HookOutput) -> ! {
    let json = serde_json::to_string(&output).unwrap_or_else(|_| "{}".into());
    println!("{json}");
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_pi_hook_output_allow() {
        let output = serde_json::to_string(&PiHookOutput::allow()).unwrap();
        assert_eq!(output, r#"{"decision":"allow"}"#);
    }

    #[test]
    fn test_pi_canonicalizes_before_policy_eval() {
        let tool_call = AgentBackend::Pi
            .canonicalize_tool_call("write", json!({"path": "foo.txt", "content": "hi"}))
            .unwrap();
        assert_eq!(tool_call.tool_name, "Write");
        assert_eq!(
            tool_call.tool_input,
            r#"{"content":"hi","file_path":"foo.txt"}"#
        );
    }
}
