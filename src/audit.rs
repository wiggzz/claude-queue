use crate::config;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, Write};

#[derive(Debug, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp: String,
    pub session_id: String,
    pub tool_name: String,
    pub tool_input: String,
    pub decision: String, // "approve" | "deny" | "escalate"
    pub reason: String,
    pub actor: String, // "supervisor" | "human" | "policy"
}

pub fn log(
    session_id: &str,
    tool_name: &str,
    tool_input: &str,
    decision: &str,
    reason: &str,
    actor: &str,
) {
    let truncated_input = if tool_input.len() > 500 {
        format!("{}...", &tool_input[..497])
    } else {
        tool_input.to_string()
    };

    let entry = AuditEntry {
        timestamp: chrono_iso8601_now(),
        session_id: session_id.to_string(),
        tool_name: tool_name.to_string(),
        tool_input: truncated_input,
        decision: decision.to_string(),
        reason: reason.to_string(),
        actor: actor.to_string(),
    };

    if let Err(e) = append_entry(&entry) {
        eprintln!("[cq audit] failed to write audit log: {e}");
    }
}

fn append_entry(entry: &AuditEntry) -> std::io::Result<()> {
    let log_dir = config::log_dir();
    fs::create_dir_all(&log_dir)?;
    let path = log_dir.join("audit.log");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let line = serde_json::to_string(entry).map_err(|e| std::io::Error::other(e))?;
    writeln!(file, "{line}")
}

fn chrono_iso8601_now() -> String {
    // Use std::time to get UTC timestamp in ISO 8601 format
    let now = std::time::SystemTime::now();
    let duration = now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = duration.as_secs();

    // Convert to date/time components
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;

    // Days since epoch to year/month/day (simplified algorithm)
    let (year, month, day) = days_to_ymd(days);

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn days_to_ymd(days_since_epoch: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days_since_epoch + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Read the last N entries from the audit log.
pub fn read_tail(n: usize) -> Vec<AuditEntry> {
    let path = config::log_dir().join("audit.log");
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = std::io::BufReader::new(file);
    let mut entries: Vec<AuditEntry> = Vec::new();
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<AuditEntry>(&line) {
            entries.push(entry);
        }
    }
    // Return only last N
    if entries.len() > n {
        entries.split_off(entries.len() - n)
    } else {
        entries
    }
}
