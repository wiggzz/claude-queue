use crate::backend::AgentBackend;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "cq",
    version,
    about = "Claude Queue — orchestrate coding-agent sub-agent sessions",
    long_about = "\
Orchestrate parallel coding-agent sub-agents with tool-call permission gating.

QUICK START:
  1. cq push auth-fix \"fix the auth bug\" --cwd ~/myproject
  2. cq push tests \"add tests\" --cwd ~/myproject
  3. cq pending                    # tool calls waiting for approval
  4. cq approve all                # approve everything pending
  5. cq list                       # check session statuses
  6. cq tail auth-fix             # get session output (by name or ID prefix)
  7. cq push auth-fix \"now fix the edge case too\"

BEST PRACTICES FOR ORCHESTRATORS:
  - Use cq push <name> — it starts, queues, or resumes as needed
  - Use --cwd to set each sub-agent's working directory
  - For agents editing overlapping files, use git worktrees:
      git worktree add ../my-worktree && cq start \"...\" --cwd ../my-worktree
  - cq start returns immediately — do NOT call cq wait or
    cq pending --wait in your main loop (they block and will deadlock)
  - Poll with: cq list (statuses) and cq pending (approvals) — both instant
  - Batch approve with filters:
      cq approve all [--session <name>] [--tool <tool>] [--match <regex>]
  - Get output: cq tail <name> once a session completes

POLICIES & SUPERVISOR:
  Policies control which tools are auto-approved, denied, or escalated.
  Config files (project takes priority over user):
    Project: .cq/config.json    User: ~/.cq/config.json
  Enable the supervisor in config for LLM-driven auto-approve/deny.
  View active stack: cq policy list

MONITORING:
  cq tail               Show recent messages from running sessions
  cq tail <name> -f     Follow a session's output in real-time
  cq watch              Live dashboard (sessions + pending approvals)
  cq audit --follow     Real-time decision log (run in a background task)"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start a fresh sub-agent session (always creates new, never queues or resumes).
    /// Use `cq push` for the smart start/queue/resume behavior.
    Start {
        /// The prompt to send to the sub-agent
        prompt: String,
        /// Friendly name for this session (used with resume, tail, etc.)
        #[arg(long, short)]
        name: Option<String>,
        /// Working directory for the sub-agent (default: current dir)
        #[arg(long, default_value = ".")]
        cwd: String,
        /// Backend to use (default: --backend, CQ_AGENT_BACKEND, config default_backend, then claude)
        #[arg(long, value_enum)]
        backend: Option<AgentBackend>,
    },
    /// Push a message to a session: starts, queues, or resumes as needed.
    ///
    /// If no session exists with this name, starts a new one.
    /// If the session is running, queues the message for delivery when it finishes.
    /// If the session is completed/failed, resumes it with the message.
    /// Multiple pushes accumulate — all queued messages are delivered together.
    Push {
        /// Session name (required — this is the stable handle for the work stream)
        name: String,
        /// Message to send to the agent
        prompt: String,
        /// Working directory for the sub-agent (default: current dir)
        #[arg(long, default_value = ".")]
        cwd: String,
        /// Cancel all queued messages for this session
        #[arg(long)]
        cancel: bool,
    },
    /// Interrupt a running session: kill it, clear the queue, and resume with a new message.
    Interrupt {
        /// Session name
        name: String,
        /// Message to send after killing
        prompt: String,
        /// Working directory for the sub-agent (default: current dir)
        #[arg(long, default_value = ".")]
        cwd: String,
    },
    /// List all sessions with their status (running, completed, failed)
    List {
        /// Filter by session name (contains match) or session ID (prefix match)
        #[arg(long)]
        session: Option<String>,
        /// Filter by status (e.g. running, completed, failed, killed)
        #[arg(long)]
        status: Option<String>,
    },
    /// Show tool calls waiting for your approval
    #[command(args_conflicts_with_subcommands = true)]
    Pending {
        /// Filter by session ID (prefix match)
        #[arg(long)]
        session: Option<String>,
        /// Block until a new pending tool call appears, print it, then exit.
        /// WARNING: This is blocking! Only use in background tasks or scripts,
        /// never in an orchestrator's main loop. Use 'cq pending' (without --wait)
        /// for non-blocking checks.
        #[arg(long, short)]
        wait: bool,
        /// Output pending calls as JSON Lines (one JSON object per line)
        #[arg(long)]
        json: bool,
        /// Show full (untruncated) tool input for all pending calls
        #[arg(long)]
        full: bool,
        #[command(subcommand)]
        command: Option<PendingCommands>,
    },
    /// Approve a pending tool call: cq approve <id|summary|all> [--session <name>] [--tool <name>] [--match <regex>]
    Approve {
        /// Tool call ID (number), escalation summary (text), or "all" to approve everything pending
        id: String,
        /// Only approve tool calls for this session (name or ID prefix)
        #[arg(long)]
        session: Option<String>,
        /// Only approve tool calls for this tool type (e.g. "Bash", "Edit", "Write")
        #[arg(long)]
        tool: Option<String>,
        /// Only approve tool calls whose tool_input matches this regex (for Bash, matches the "command" field)
        #[arg(long = "match")]
        match_pattern: Option<String>,
    },
    /// Deny a pending tool call: cq deny <id> [--reason "..."]
    Deny {
        /// Tool call ID to deny
        id: i64,
        /// Reason for denial (shown to the sub-agent)
        #[arg(long)]
        reason: Option<String>,
    },
    /// Show session messages (like tail for your agents)
    ///
    /// Without a session argument, shows recent messages from all running sessions.
    /// With a session name, shows the full output across that session's resume chain.
    /// With a session ID/backend ID prefix, shows the matching session.
    /// Use --follow to keep streaming new messages as they arrive.
    Tail {
        /// Session name or ID prefix
        session: Option<String>,
        /// Number of recent messages to show
        ///
        /// Defaults to 20 when tailing all running sessions, or all messages when a session is specified.
        #[arg(short = 'n', long)]
        num: Option<usize>,
        /// Keep streaming new messages as they arrive
        #[arg(long, short)]
        follow: bool,
        /// Output as JSON Lines
        #[arg(long)]
        json: bool,
    },
    /// Resume a session: cq resume <name-or-id> ["follow-up prompt"]
    ///
    /// Takes a session name, cq ID prefix, or raw backend session ID.
    Resume {
        /// Session name, cq ID prefix, or full backend session ID
        session_id: String,
        /// Follow-up prompt to send (default: "continue")
        #[arg(default_value = "continue")]
        prompt: String,
        /// Working directory (default: current dir)
        #[arg(long, default_value = ".")]
        cwd: String,
        /// Backend to use for raw external session IDs
        #[arg(long, value_enum)]
        backend: Option<AgentBackend>,
    },
    /// Block until a session completes: cq wait <name-or-id>
    ///
    /// WARNING: This is blocking! Only use in background tasks or scripts,
    /// never in an orchestrator's main loop. Use 'cq list' to check status
    /// non-blockingly.
    Wait {
        /// Session name or ID (prefix match)
        session_id: String,
    },
    /// Kill a running sub-agent session
    Kill {
        /// Session ID (prefix match)
        session_id: String,
    },
    /// Live dashboard: sessions + pending approvals, refreshes every 2s
    Watch,
    /// Discover and search non-cq-managed Claude Code sessions (Claude-only)
    Sessions {
        #[command(subcommand)]
        command: SessionsCommands,
    },
    /// Manage auto-approve/deny policies for tool calls
    Policy {
        #[command(subcommand)]
        command: PolicyCommands,
    },
    /// View the audit log of supervisor and human approval decisions
    Audit {
        /// Number of entries to show (default: 20)
        #[arg(long, default_value = "20")]
        tail: usize,
        /// Output as raw JSON Lines
        #[arg(long)]
        json: bool,
        /// Follow the audit log in real-time (like tail -f)
        #[arg(long, short)]
        follow: bool,
        /// Show full tool call details (command, file path, input)
        #[arg(long, short)]
        verbose: bool,
    },
    /// Clean up expired sessions and old data: cq gc [--older-than 7d] [--dry-run]
    Gc {
        /// Remove sessions older than this duration (e.g. "7d", "24h", "30d"). Default: 7d
        #[arg(long, default_value = "7d")]
        older_than: String,
        /// Only show what would be cleaned up, don't actually delete
        #[arg(long)]
        dry_run: bool,
    },
    /// Update cq to the latest release from GitHub
    Update,
    /// Print the current version
    Version,
    /// Show or manage configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// [internal] Hook entry point called by backend-specific tool interception
    #[command(hide = true)]
    Hook {
        #[arg(default_value = "claude")]
        backend: String,
    },
    /// [internal] Background session runner
    #[command(hide = true)]
    RunSession {
        session_id: String,
        #[arg(long, value_enum)]
        backend: AgentBackend,
        #[arg(long)]
        agent_session_id: String,
        #[arg(long)]
        cwd: String,
        #[arg(long)]
        prompt_display: String,
        #[arg(long)]
        prompt: String,
        #[arg(long)]
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum PendingCommands {
    /// Show full details for a specific tool call by ID
    Show {
        /// Tool call ID
        id: i64,
    },
}

#[derive(Subcommand)]
pub enum SessionsCommands {
    /// List recent Claude Code sessions (not managed by cq, Claude-only)
    List {
        /// Maximum number of sessions to show
        #[arg(long, short, default_value = "20")]
        limit: usize,
    },
    /// Search session content for a string (e.g. ticket ID, file path, PR number)
    Search {
        /// Text to search for across all session content
        query: String,
    },
    /// Show details for a specific session
    Show {
        /// Session ID (prefix match)
        session_id: String,
    },
}

#[derive(Subcommand)]
pub enum PolicyCommands {
    /// Show all policies (project policies listed first, take priority)
    List,
    /// Add a policy: cq policy add <tool> <allow|deny|ask> [--user]
    Add {
        /// Tool name or pattern (e.g. "Edit", "Bash", "mcp__*", "*")
        tool: String,
        /// Action: "allow" (auto-approve), "deny" (auto-reject), "ask" (require manual approval)
        action: String,
        /// Save to user config (~/.cq/config.json) instead of project config
        #[arg(long)]
        user: bool,
        /// Regex pattern to match against tool_input (e.g. "rm -rf" for Bash)
        #[arg(long)]
        pattern: Option<String>,
    },
    /// Remove a policy: cq policy remove <tool> [--user]
    Remove {
        /// Tool name pattern to remove
        tool: String,
        /// Remove from user config instead of project config
        #[arg(long)]
        user: bool,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Show the effective merged configuration (user + project + defaults)
    Show,
}

#[cfg(test)]
mod tests {
    use super::{Cli, Commands};
    use clap::{CommandFactory, Parser};

    #[test]
    fn test_long_help_keeps_push_first_workflow() {
        let mut cmd = Cli::command();
        let mut rendered = Vec::new();
        cmd.write_long_help(&mut rendered).unwrap();
        let help = String::from_utf8(rendered).unwrap();

        assert!(help.contains("cq push auth-fix \"fix the auth bug\" --cwd ~/myproject"));
        assert!(help.contains("Use cq push <name>"));
        assert!(help.contains("cq push auth-fix \"now fix the edge case too\""));
        assert!(help.contains("Get output: cq tail <name>"));
        assert!(!help.contains("cq result"));
    }

    #[test]
    fn test_tail_accepts_session_as_positional_argument() {
        let cli = Cli::try_parse_from(["cq", "tail", "agent-name", "-f"]).unwrap();
        match cli.command {
            Commands::Tail {
                session,
                follow,
                num,
                json,
            } => {
                assert_eq!(session.as_deref(), Some("agent-name"));
                assert!(follow);
                assert_eq!(num, None);
                assert!(!json);
            }
            _ => panic!("expected Tail command"),
        }
    }

    #[test]
    fn test_result_command_is_rejected() {
        let parsed = Cli::try_parse_from(["cq", "result", "agent-name"]);
        assert!(parsed.is_err());
        let rendered = parsed.err().unwrap().to_string();
        assert!(rendered.contains("unrecognized subcommand 'result'"));
    }
}
