use crate::config;
use crate::db::Db;
use crate::format;
use std::io::Write;

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = config::db_path();

    loop {
        // Clear screen
        print!("\x1b[2J\x1b[H");
        std::io::stdout().flush()?;

        let db = Db::open(&db_path)?;
        // Proactively resolve dead sessions each refresh cycle
        crate::session::resolve_running_sessions(&db);

        // Sessions
        let sessions = db.get_sessions()?;
        println!("\x1b[1m=== Sessions ===\x1b[0m");
        if sessions.is_empty() {
            println!("  (none)");
        } else {
            println!(
                "  {:<8} {:<10} {:<8} {:<20} PROMPT",
                "ID", "STATUS", "PID", "STARTED"
            );
            for s in &sessions {
                let status_display = &s.status;
                let prompt_short = if s.prompt.len() > 50 {
                    format!("{}...", &s.prompt[..47])
                } else {
                    s.prompt.clone()
                };
                println!(
                    "  {:<8} {:<10} {:<8} {:<20} {}",
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
            println!(
                "  {:<6} {:<10} {:<15} {:<20} INPUT",
                "ID", "SESSION", "TOOL", "SINCE"
            );
            for tc in &pending {
                let input_short = format::format_tool_input(&tc.tool_name, &tc.tool_input, 40);
                println!(
                    "  {:<6} {:<10} {:<15} {:<20} {}",
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
