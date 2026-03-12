use crate::audit;
use crate::config::{self, Config};
use crate::db::Db;
use crate::policy;
use crate::supervisor;
use serde::{Deserialize, Serialize};
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

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Only activate for managed sessions
    if std::env::var("CQ_MANAGED").is_err() {
        print_and_exit(HookOutput::allow());
    }

    // Read hook input from stdin
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let hook_input: HookInput = serde_json::from_str(&input)?;

    let session_id = hook_input.session_id.unwrap_or_else(|| "unknown".into());
    let tool_name = hook_input.tool_name.unwrap_or_default();
    let tool_input_str = hook_input
        .tool_input
        .map(|v| serde_json::to_string(&v).unwrap_or_default())
        .unwrap_or_default();

    // Load config — use CQ_PROJECT_DIR (set by cq start) to find the project root,
    // which handles git worktrees correctly. Falls back to cwd.
    let project_dir = std::env::var("CQ_PROJECT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let config = Config::load(&project_dir);

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
                print_and_exit(HookOutput::allow());
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
                print_and_exit(HookOutput::deny(Some(format!(
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
                print_and_exit(HookOutput::allow());
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
            print_and_exit(HookOutput::deny(Some("Approval timed out".into())));
        }

        if let Some((status, reason)) = db.get_tool_call_status(tc_id)? {
            match status.as_str() {
                "approved" => print_and_exit(HookOutput::allow()),
                "denied" => print_and_exit(HookOutput::deny(reason)),
                "timed_out" => print_and_exit(HookOutput::deny(Some("Timed out".into()))),
                "pending" => continue,
                _ => continue,
            }
        }
    }
}

fn print_and_exit(output: HookOutput) -> ! {
    let json = serde_json::to_string(&output).unwrap_or_else(|_| "{}".into());
    println!("{json}");
    std::process::exit(0);
}
