use crate::backend::AgentBackend;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "cq",
    version,
    about = "Claude Queue — orchestrate coding-agent sub-sessions with tool-call gating",
    long_about = "\
Orchestrate parallel coding-agent sub-sessions with tool-call permission gating.

QUICK START:
  1. cq start \"fix the auth bug\" --name auth-fix --cwd ~/myproject
  2. cq start \"add tests\" --name tests --cwd ~/myproject
  3. cq pending                    # tool calls waiting for approval
  4. cq approve all                # approve everything pending
  5. cq list                       # check session statuses
  6. cq result auth-fix            # get final output (by name or ID prefix)
  7. cq resume auth-fix \"now fix the edge case too\"

BEST PRACTICES FOR ORCHESTRATORS:
  - Always use --name so you can refer to sessions later
  - Use --cwd to set each sub-agent's working directory
  - For agents editing overlapping files, use git worktrees:
      git worktree add ../my-worktree && cq start \"...\" --cwd ../my-worktree
  - cq start returns immediately — do NOT call cq wait or
    cq pending --wait in your main loop (they block and will deadlock)
  - Poll with: cq list (statuses) and cq pending (approvals) — both instant
  - Batch approve with filters:
      cq approve all [--session <name>] [--tool <tool>] [--match <regex>]
  - Get output: cq result <name> once a session completes

POLICIES & SUPERVISOR:
  Policies control which tools are auto-approved, denied, or escalated.
  Config files (project takes priority over user):
    Project: .cq/config.json    User: ~/.cq/config.json
  Enable the supervisor in config for LLM-driven auto-approve/deny.
  View active stack: cq policy list

MONITORING:
  cq watch              Live dashboard (sessions + pending approvals)
  cq audit --follow     Real-time decision log (run in a background task)"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start a new sub-agent: cq start "your prompt here" [--name my-task] [--cwd DIR]
    Start {
        /// The prompt to send to the sub-agent
        prompt: String,
        /// Friendly name for this session (used with resume, result, etc.)
        #[arg(long, short)]
        name: Option<String>,
        /// Working directory for the sub-agent (default: current dir)
        #[arg(long, default_value = ".")]
        cwd: String,
        /// Agent backend to use (default: config default_agent, which defaults to claude)
        #[arg(long, value_enum)]
        agent: Option<AgentBackend>,
    },
    /// List all sessions with their status (running, completed, failed)
    List {
        /// Filter by session name (contains match) or session ID (prefix match)
        #[arg(long)]
        session: Option<String>,
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
    /// Get the final text output from a completed session
    Result {
        /// Session ID (prefix match — first 8 chars is enough)
        session_id: String,
    },
    /// View the raw output log, optionally following in real-time
    Output {
        /// Session ID (prefix match)
        session_id: String,
        /// Stream output as it's written (like tail -f)
        #[arg(long, short)]
        follow: bool,
    },
    /// Resume a session: cq resume <name-or-id> ["follow-up prompt"]
    ///
    /// Takes a session name, cq ID prefix, or raw Claude session ID.
    Resume {
        /// Session name, cq ID prefix, or full Claude session ID
        session_id: String,
        /// Follow-up prompt to send (default: "continue")
        #[arg(default_value = "continue")]
        prompt: String,
        /// Working directory (default: current dir)
        #[arg(long, default_value = ".")]
        cwd: String,
        /// Agent backend to use for raw external session IDs
        #[arg(long, value_enum)]
        agent: Option<AgentBackend>,
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
    /// Discover and search non-cq-managed Claude Code sessions
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
    /// Update cq to the latest release from GitHub
    Update,
    /// Print the current version
    Version,
    /// [internal] Hook entry point called by backend-specific tool interception
    #[command(hide = true)]
    Hook {
        #[arg(default_value = "claude")]
        agent: String,
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
    /// List recent Claude Code sessions (not managed by cq)
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
