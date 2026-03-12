use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "cq",
    about = "Claude Queue — orchestrate multiple Claude Code sub-agent sessions",
    long_about = "\
Claude Queue (cq) lets you run multiple Claude Code sessions in parallel and \
control their tool permissions from a single place.

QUICK START (orchestrator workflow):
  1. cq start \"fix the auth bug\" --name auth-fix --cwd ~/myproject
  2. cq start \"add tests\" --name tests --cwd ~/myproject
  3. cq pending                          # see which tool calls need approval
  4. cq approve all                      # approve all pending tool calls
  5. cq list                             # check session statuses
  6. cq result auth-fix                  # get the final output (by name or ID prefix)
  7. cq resume auth-fix \"now fix the edge case too\"  # continue a session

TOOL CALL GATING:
  Sub-agents run with all permissions bypassed — instead, a hook intercepts \
  every tool call and checks it against your policies (cq policy list). \
  Read-only tools (Read, Glob, Grep) are auto-approved by default. \
  Everything else blocks until you run 'cq approve <id>' or 'cq deny <id>'.

POLICIES:
  User-level:    ~/.cq/config.json
  Project-level: .cq/config.json (in project root, takes priority)
  Manage with:   cq policy list | cq policy add <tool> <action>"
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
        /// Block until a new pending tool call appears, print it, then exit
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
    /// Approve a pending tool call: cq approve <id> or cq approve all [--session <name>] [--tool <name>]
    Approve {
        /// Tool call ID (number) or "all" to approve everything pending
        id: String,
        /// Only approve tool calls for this session (name or ID prefix)
        #[arg(long)]
        session: Option<String>,
        /// Only approve tool calls for this tool type (e.g. "Bash", "Edit", "Write")
        #[arg(long)]
        tool: Option<String>,
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
    /// [internal] Hook entry point called by Claude Code's PreToolUse system
    #[command(hide = true)]
    Hook,
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
