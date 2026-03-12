use crate::config;
use crate::db;
use crate::format;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    let line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    writeln!(file, "{line}")
}

fn chrono_iso8601_now() -> String {
    // Use std::time to get UTC timestamp in ISO 8601 format
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
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

/// Follow the audit log in real-time, printing new entries as they appear.
/// Prints the last `tail` entries first, then polls for new lines every 500ms.
pub fn follow(tail: usize, json: bool) {
    use std::io::{Seek, SeekFrom};

    let path = config::log_dir().join("audit.log");
    let session_names = load_session_names();

    // Print initial tail entries
    let entries = read_tail(tail);
    if !entries.is_empty() {
        if !json {
            println!(
                "{:<22} {:<10} {:<10} {:<15} {:<10} REASON",
                "TIMESTAMP", "DECISION", "ACTOR", "TOOL", "SESSION"
            );
        }
        for entry in &entries {
            print_entry(entry, json, &session_names);
        }
    }

    // Open file and seek to end
    let mut file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => {
            // File doesn't exist yet — wait for it
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if let Ok(f) = fs::File::open(&path) {
                    if !json && entries.is_empty() {
                        println!(
                            "{:<22} {:<10} {:<10} {:<15} {:<10} REASON",
                            "TIMESTAMP", "DECISION", "ACTOR", "TOOL", "SESSION"
                        );
                    }
                    break f;
                }
            }
        }
    };
    file.seek(SeekFrom::End(0)).unwrap_or_default();

    let mut header_printed = !entries.is_empty();
    let mut partial_line = String::new();

    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));

        let mut buf = Vec::new();
        use std::io::Read;
        if file.read_to_end(&mut buf).unwrap_or(0) == 0 {
            continue;
        }

        let text = String::from_utf8_lossy(&buf);
        partial_line.push_str(&text);

        while let Some(newline_pos) = partial_line.find('\n') {
            let line: String = partial_line.drain(..=newline_pos).collect();
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<AuditEntry>(line) {
                if !json && !header_printed {
                    println!(
                        "{:<22} {:<10} {:<10} {:<15} {:<10} REASON",
                        "TIMESTAMP", "DECISION", "ACTOR", "TOOL", "SESSION"
                    );
                    header_printed = true;
                }
                print_entry(&entry, json, &session_names);
            }
        }
    }
}

/// Load a session_id -> name map from the database. Returns an empty map on failure.
pub fn load_session_names() -> HashMap<String, String> {
    db::Db::open(&config::db_path())
        .and_then(|db| db.get_session_names())
        .unwrap_or_default()
}

/// Resolve a session_id to a display name: use the session name if available,
/// otherwise truncate the session_id to 8 characters.
fn session_display(session_id: &str, names: &HashMap<String, String>) -> String {
    if let Some(name) = names.get(session_id) {
        name.clone()
    } else if session_id.len() > 8 {
        session_id[..8].to_string()
    } else {
        session_id.to_string()
    }
}

pub fn print_entry(entry: &AuditEntry, json: bool, session_names: &HashMap<String, String>) {
    if json {
        println!("{}", serde_json::to_string(entry).unwrap());
    } else {
        let session_short = session_display(&entry.session_id, session_names);
        let reason_short = if entry.reason.len() > 40 {
            format!("{}...", &entry.reason[..37])
        } else {
            entry.reason.clone()
        };
        let tool_display = format::format_tool_input(&entry.tool_name, &entry.tool_input, 60);
        println!(
            "{:<22} {:<10} {:<10} {:<15} {:<10} {}",
            &entry.timestamp,
            entry.decision,
            entry.actor,
            entry.tool_name,
            session_short,
            reason_short,
        );
        if tool_display != entry.tool_input {
            // Tool input was formatted specially (Bash command, file path, etc.)
            println!("  {tool_display}");
        } else if !entry.tool_input.is_empty() {
            let input_short = if entry.tool_input.len() > 80 {
                format!("{}...", &entry.tool_input[..77])
            } else {
                entry.tool_input.clone()
            };
            println!("  {input_short}");
        }
    }
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
