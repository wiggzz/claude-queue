use crate::audit;
use crate::config::{self, Config};
use crate::db::Db;
use crate::policy;
use crate::supervisor;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
struct HookInput {
    session_id: Option<String>,
    tool_name: Option<String>,
    tool_input: Option<serde_json::Value>,
    #[allow(dead_code)]
    #[serde(flatten)]
    _extra: serde_json::Map<String, serde_json::Value>,
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

enum Decision {
    Allow,
    Deny(Option<String>),
}

pub fn run(agent: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    match agent.unwrap_or("claude") {
        "claude" => run_claude(),
        "codex-shell" => run_codex_shell(),
        other => Err(format!("Unknown hook backend '{other}'").into()),
    }
}

fn run_claude() -> Result<(), Box<dyn std::error::Error>> {
    // Only activate for managed sessions
    if std::env::var("CQ_MANAGED").is_err() {
        print_and_exit(HookOutput::allow());
    }

    // Read hook input from stdin
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let hook_input: HookInput = serde_json::from_str(&input)?;

    let session_id = managed_session_id(hook_input.session_id);
    let tool_name = hook_input.tool_name.unwrap_or_default();
    let tool_input_str = hook_input
        .tool_input
        .map(|v| serde_json::to_string(&v).unwrap_or_default())
        .unwrap_or_default();

    match evaluate_tool_call(&session_id, &tool_name, &tool_input_str)? {
        Decision::Allow => print_and_exit(HookOutput::allow()),
        Decision::Deny(reason) => print_and_exit(HookOutput::deny(reason)),
    }
}

fn run_codex_shell() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("CQ_MANAGED").is_err() {
        return Ok(());
    }

    let session_id = managed_session_id(None);
    let command = std::env::var("ZSH_EXECUTION_STRING").unwrap_or_default();
    if command.trim().is_empty() {
        return Ok(());
    }

    let tool_input_str = serde_json::to_string(&json!({ "command": command }))?;

    match evaluate_tool_call(&session_id, "Bash", &tool_input_str)? {
        Decision::Allow => Ok(()),
        Decision::Deny(reason) => {
            if let Some(reason) = reason {
                eprintln!("{reason}");
            }
            std::process::exit(1);
        }
    }
}

fn evaluate_tool_call(
    session_id: &str,
    tool_name: &str,
    tool_input_str: &str,
) -> Result<Decision, Box<dyn std::error::Error>> {
    let config = load_config();

    if let Some(decision) = policy::check(&tool_name, &tool_input_str, &config.policies) {
        match decision.as_str() {
            "allow" => {
                audit::log(
                    &session_id,
                    &tool_name,
                    &tool_input_str,
                    "approve",
                    "Allowed by policy",
                    "policy",
                );
                return Ok(Decision::Allow);
            }
            "deny" => {
                audit::log(
                    &session_id,
                    &tool_name,
                    &tool_input_str,
                    "deny",
                    &format!("Denied by policy for tool: {tool_name}"),
                    "policy",
                );
                return Ok(Decision::Deny(Some(format!(
                    "Denied by policy for tool: {tool_name}"
                ))));
            }
            _ => {}
        }
    }

    // No static policy match — try supervisor if enabled
    let mut escalation_summary: Option<String> = None;
    if config.supervisor.enabled {
        match supervisor::evaluate(&config.supervisor, &tool_name, &tool_input_str) {
            Ok(supervisor::Decision::Approve(reason)) => {
                eprintln!("[cq supervisor] approved: {reason}");
                audit::log(
                    &session_id,
                    &tool_name,
                    &tool_input_str,
                    "approve",
                    &reason,
                    "supervisor",
                );
                return Ok(Decision::Allow);
            }
            Ok(supervisor::Decision::Escalate { reason, summary }) => {
                eprintln!("[cq supervisor] escalated: {reason}");
                if let Some(ref s) = summary {
                    eprintln!("[cq supervisor] summary: {s}");
                }
                audit::log(
                    &session_id,
                    &tool_name,
                    &tool_input_str,
                    "escalate",
                    &reason,
                    "supervisor",
                );
                escalation_summary = summary;
                // Fall through to human approval
            }
            Err(e) => {
                eprintln!("[cq supervisor] error, escalating: {e}");
                // Fall through to human approval
            }
        }
    }

    // No auto-decision — register in DB and wait for approval
    let db_path = config::db_path();
    let db = Db::open(&db_path)?;
    let tc_id = db.insert_tool_call_with_summary(
        &session_id,
        &tool_name,
        &tool_input_str,
        escalation_summary.as_deref(),
    )?;

    // Poll for resolution
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
