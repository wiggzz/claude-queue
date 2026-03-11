use crate::config;
use crate::db::Db;
use crate::session;
use std::io::Write;

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = config::db_path();

    loop {
        // Clear screen
        print!("\x1b[2J\x1b[H");
        std::io::stdout().flush()?;

        let db = Db::open(&db_path)?;

        // Sessions
        let sessions = db.get_sessions()?;
        println!("\x1b[1m=== Sessions ===\x1b[0m");
        if sessions.is_empty() {
            println!("  (none)");
        } else {
            println!("  {:<8} {:<10} {:<8} {:<20} {}",
                "ID", "STATUS", "PID", "STARTED", "PROMPT");
            for s in &sessions {
                let alive = s.pid.map(|p| session::is_pid_alive(p)).unwrap_or(false);
                let status_display = if s.status == "running" && !alive {
                    "dead?"
                } else {
                    &s.status
                };
                let prompt_short = if s.prompt.len() > 50 {
                    format!("{}...", &s.prompt[..47])
                } else {
                    s.prompt.clone()
                };
                println!("  {:<8} {:<10} {:<8} {:<20} {}",
                    &s.session_id[..8],
                    status_display,
                    s.pid.map(|p| p.to_string()).unwrap_or("-".into()),
                    &s.started_at,
                    prompt_short,
                );
            }
        }

        println!();

        // Pending approvals
        let pending = db.get_pending_tool_calls(None)?;
        println!("\x1b[1m=== Pending Approvals ===\x1b[0m");
        if pending.is_empty() {
            println!("  (none)");
        } else {
            println!("  {:<6} {:<10} {:<15} {:<20} {}",
                "ID", "SESSION", "TOOL", "SINCE", "INPUT");
            for tc in &pending {
                let input_short = if tc.tool_input.len() > 40 {
                    format!("{}...", &tc.tool_input[..37])
                } else {
                    tc.tool_input.clone()
                };
                println!("  {:<6} {:<10} {:<15} {:<20} {}",
                    tc.id,
                    &tc.session_id[..8.min(tc.session_id.len())],
                    tc.tool_name,
                    &tc.created_at,
                    input_short,
                );
            }
        }

        println!("\n\x1b[2mRefreshing every 2s. Press Ctrl-C to exit.\x1b[0m");

        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}
