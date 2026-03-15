use crate::config;
use crate::db::{Db, Session};
use crate::format;
use crate::session;
use std::io::Write;

pub fn run(show_all: bool) -> Result<(), Box<dyn std::error::Error>> {
    let db_path = config::db_path();

    loop {
        // Clear screen
        print!("\x1b[2J\x1b[H");
        std::io::stdout().flush()?;

        let db = Db::open(&db_path)?;

        // Sessions
        let all_sessions = db.get_sessions()?;
        let sessions = filtered_sessions(&all_sessions, show_all);
        println!("\x1b[1m=== Sessions ===\x1b[0m");
        if sessions.is_empty() {
            println!("  (none)");
        } else {
            println!(
                "  {:<8} {:<10} {:<8} {:<20} PROMPT",
                "ID", "STATUS", "PID", "STARTED"
            );
            for s in &sessions {
                let alive = s.pid.map(session::is_pid_alive).unwrap_or(false);
                let status_display = if s.status == "running" && !alive {
                    "dead?"
                } else {
                    &s.status
                };
                let prompt_short = crate::main_prompt_preview(&s.prompt, 50);
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

fn filtered_sessions(sessions: &[Session], show_all: bool) -> Vec<&Session> {
    sessions
        .iter()
        .filter(|s| show_all || !is_old_completed_session(s))
        .collect()
}

fn is_old_completed_session(s: &Session) -> bool {
    matches!(s.status.as_str(), "completed" | "failed" | "killed")
        && match completed_at_age_minutes(s) {
            Some(age) => age > 5,
            None => false,
        }
}

fn completed_at_age_minutes(s: &Session) -> Option<i64> {
    let completed_at = s._completed_at.as_deref()?;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let completed_secs = sqlite_datetime_to_epoch_seconds(completed_at)?;
    Some((now_secs - completed_secs) / 60)
}

fn sqlite_datetime_to_epoch_seconds(s: &str) -> Option<i64> {
    let mut parts = s.split(['-', ' ', ':']);
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    let hour: u32 = parts.next()?.parse().ok()?;
    let minute: u32 = parts.next()?.parse().ok()?;
    let second: u32 = parts.next()?.parse().ok()?;

    let days = days_from_civil(year, month, day)?;
    Some(days * 86_400 + hour as i64 * 3_600 + minute as i64 * 60 + second as i64)
}

fn days_from_civil(year: i32, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = month as i32 + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day as i32 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::AgentBackend;

    fn session(status: &str, completed_at: Option<&str>) -> Session {
        Session {
            _id: 1,
            session_id: "sess-12345678".into(),
            agent_backend: AgentBackend::Claude,
            agent_session_id: Some("sess-12345678".into()),
            claude_session_id: Some("sess-12345678".into()),
            name: None,
            prompt: "prompt".into(),
            _cwd: "/tmp".into(),
            status: status.into(),
            pid: Some(1),
            started_at: "2026-03-15 12:00:00".into(),
            _completed_at: completed_at.map(str::to_string),
            _exit_code: Some(0),
        }
    }

    #[test]
    fn hides_old_completed_sessions_by_default() {
        let sessions = vec![
            session("running", None),
            session("completed", Some("2000-01-01 00:00:00")),
        ];

        let filtered = filtered_sessions(&sessions, false);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].status, "running");
    }

    #[test]
    fn show_all_keeps_old_completed_sessions() {
        let sessions = vec![
            session("running", None),
            session("completed", Some("2000-01-01 00:00:00")),
        ];

        let filtered = filtered_sessions(&sessions, true);
        assert_eq!(filtered.len(), 2);
    }
}
