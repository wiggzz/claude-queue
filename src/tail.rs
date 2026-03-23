use crate::backend::AgentBackend;
use crate::config;
use crate::db::Db;
use crate::format;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// ANSI colors for session labels (cycles through these).
const COLORS: &[&str] = &[
    "\x1b[36m", // cyan
    "\x1b[33m", // yellow
    "\x1b[35m", // magenta
    "\x1b[32m", // green
    "\x1b[34m", // blue
    "\x1b[31m", // red
];
const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";

/// State for tailing a single JSONL file.
struct TailState {
    backend: AgentBackend,
    path: PathBuf,
    offset: u64,
    session_name: String,
    color: &'static str,
}

struct SessionSource {
    backend: AgentBackend,
    name: String,
    path: PathBuf,
}

/// Show the last N messages from session(s), optionally following for new ones.
pub fn run(
    session_filter: Option<&str>,
    num_messages: Option<usize>,
    follow: bool,
    json_mode: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let db = Db::open(&config::db_path())?;
    let mut sessions = find_sessions(&db, session_filter)?;

    // In follow mode, wait for sessions to appear if none found yet
    if sessions.is_empty() && follow {
        if !json_mode {
            if let Some(filter) = session_filter {
                eprintln!("{DIM}Waiting for sessions matching '{filter}'...{RESET}");
            } else {
                eprintln!("{DIM}Waiting for sessions...{RESET}");
            }
        }
        loop {
            std::thread::sleep(Duration::from_millis(500));
            sessions = find_sessions(&db, session_filter)?;
            if !sessions.is_empty() {
                break;
            }
        }
    }

    if sessions.is_empty() {
        if let Some(filter) = session_filter {
            eprintln!("No sessions matching '{filter}'");
        } else {
            eprintln!("No sessions found.");
        }
        return Ok(());
    }

    let mut color_idx: usize = 0;
    let multi_session = sessions.len() > 1;
    let num_messages = num_messages.unwrap_or_else(|| {
        if session_filter.is_some() {
            usize::MAX
        } else {
            20
        }
    });

    // Show last N messages from each session
    for source in &sessions {
        let color = COLORS[color_idx % COLORS.len()];
        color_idx += 1;

        let events = read_last_n_events(source.backend, &source.path, num_messages)?;
        for event in &events {
            if json_mode {
                print_json_event(&source.name, event);
            } else {
                print_event(color, &source.name, event, multi_session);
            }
        }
    }

    if !follow {
        return Ok(());
    }

    // Follow mode: continue tailing all sessions for new events
    if !json_mode {
        eprintln!("{DIM}Following... Press Ctrl-C to stop.{RESET}");
    }

    let mut tails: HashMap<String, TailState> = HashMap::new();
    color_idx = 0;

    // Initialize tail states at end of each file
    for source in &sessions {
        let offset = fs::metadata(&source.path).map(|m| m.len()).unwrap_or(0);
        let color = COLORS[color_idx % COLORS.len()];
        color_idx += 1;
        tails.insert(
            source.path.to_string_lossy().to_string(),
            TailState {
                backend: source.backend,
                path: source.path.clone(),
                offset,
                session_name: source.name.clone(),
                color,
            },
        );
    }

    let mut last_scan = Instant::now();
    let scan_interval = Duration::from_secs(5);

    loop {
        // Periodically discover new sessions (for follow mode)
        if last_scan.elapsed() >= scan_interval {
            if let Ok(new_sessions) = find_sessions(&db, session_filter) {
                for source in new_sessions {
                    let key = source.path.to_string_lossy().to_string();
                    if let std::collections::hash_map::Entry::Vacant(e) = tails.entry(key) {
                        let offset = fs::metadata(&source.path).map(|m| m.len()).unwrap_or(0);
                        let color = COLORS[color_idx % COLORS.len()];
                        color_idx += 1;
                        if !json_mode {
                            eprintln!("{DIM}tracking: {}{}{RESET}", color, source.name);
                        }
                        e.insert(TailState {
                            backend: source.backend,
                            path: source.path,
                            offset,
                            session_name: source.name,
                            color,
                        });
                    }
                }
            }
            tails.retain(|_, state| state.path.exists());
            last_scan = Instant::now();
        }

        let mut had_output = false;
        for state in tails.values_mut() {
            let lines = read_new_lines(state);
            for line in lines {
                if let Some(event) = parse_event(state.backend, &line) {
                    if json_mode {
                        print_json_event(&state.session_name, &event);
                    } else {
                        print_event(state.color, &state.session_name, &event, multi_session);
                    }
                    had_output = true;
                }
            }
        }

        if had_output {
            std::io::stdout().flush().ok();
        }

        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Find sessions and their JSONL paths. Returns (display_name, jsonl_path) pairs.
fn find_sessions(
    db: &Db,
    session_filter: Option<&str>,
) -> Result<Vec<SessionSource>, Box<dyn std::error::Error>> {
    let mut results = Vec::new();

    let sessions = if let Some(filter) = session_filter {
        // Prefer exact-name matches so a resumed chain tails as one logical session.
        let by_name = db.find_sessions_by_name(filter)?;
        if !by_name.is_empty() {
            by_name
        } else if let Some(sess) = db.find_session(filter)? {
            vec![sess]
        } else {
            return Ok(results);
        }
    } else {
        // No filter — show all running sessions
        db.get_sessions()?
            .into_iter()
            .filter(|s| {
                s.status == "running" && s.pid.map(crate::session::is_pid_alive).unwrap_or(false)
            })
            .collect()
    };

    let names = db.get_session_names().unwrap_or_default();

    for s in &sessions {
        let display_name = names
            .get(&s.session_id)
            .cloned()
            .unwrap_or_else(|| s.session_id[..8.min(s.session_id.len())].to_string());

        if let Some(path) = find_session_path(s) {
            results.push(SessionSource {
                backend: s.agent_backend,
                name: display_name,
                path,
            });
        }
    }

    Ok(results)
}

fn find_session_path(session: &crate::db::Session) -> Option<PathBuf> {
    match session.agent_backend {
        AgentBackend::Claude => {
            let claude_sid = session
                .claude_session_id
                .as_deref()
                .or(session.agent_session_id.as_deref())
                .unwrap_or(&session.session_id);
            find_claude_jsonl_for_session(claude_sid)
        }
        AgentBackend::Pi => session
            .agent_session_id
            .as_ref()
            .map(PathBuf::from)
            .filter(|path| path.exists()),
    }
}

/// Read the last N parsed events from a JSONL file.
fn read_last_n_events(
    backend: AgentBackend,
    path: &PathBuf,
    n: usize,
) -> Result<Vec<StreamEvent>, Box<dyn std::error::Error>> {
    let content = fs::read_to_string(path)?;
    let events: Vec<StreamEvent> = content
        .lines()
        .filter_map(|line| parse_event(backend, line))
        .collect();

    // Take last N
    let skip = events.len().saturating_sub(n);
    Ok(events.into_iter().skip(skip).collect())
}

/// Read new lines from a JSONL file since last offset.
fn read_new_lines(state: &mut TailState) -> Vec<String> {
    let mut lines = Vec::new();
    let file = match fs::File::open(&state.path) {
        Ok(f) => f,
        Err(_) => return lines,
    };

    let current_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if current_len <= state.offset {
        return lines;
    }

    let mut reader = BufReader::new(file);
    if reader.seek(SeekFrom::Start(state.offset)).is_err() {
        return lines;
    }

    let mut buf = String::new();
    loop {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                state.offset += n as u64;
                let trimmed = buf.trim();
                if !trimmed.is_empty() {
                    lines.push(trimmed.to_string());
                }
            }
            Err(_) => break,
        }
    }

    lines
}

/// A parsed event from a JSONL line.
struct StreamEvent {
    event_type: EventType,
    timestamp: Option<String>,
}

enum EventType {
    Text(String),
    Thinking(String),
    UserText(String),
    ToolUse { name: String, input_summary: String },
    ToolResult { content: String, is_error: bool },
}

/// Parse a JSONL line into a StreamEvent, or None if it's not interesting.
fn parse_event(backend: AgentBackend, line: &str) -> Option<StreamEvent> {
    match backend {
        AgentBackend::Claude => parse_claude_event(line),
        AgentBackend::Pi => parse_pi_event(line),
    }
}

fn parse_claude_event(line: &str) -> Option<StreamEvent> {
    let val: Value = serde_json::from_str(line).ok()?;

    let timestamp = val
        .get("timestamp")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let msg_type = val.get("type").and_then(|v| v.as_str());

    match msg_type {
        Some("assistant") => {
            let message = val.get("message")?;
            let content = message.get("content")?;
            let blocks = content.as_array()?;

            for block in blocks {
                let block_type = block.get("type").and_then(|v| v.as_str())?;
                match block_type {
                    "text" => {
                        let text = block.get("text").and_then(|v| v.as_str())?;
                        if !text.trim().is_empty() {
                            return Some(StreamEvent {
                                event_type: EventType::Text(text.to_string()),
                                timestamp: timestamp.clone(),
                            });
                        }
                    }
                    "thinking" => {
                        let thinking = block.get("thinking").and_then(|v| v.as_str())?;
                        if !thinking.trim().is_empty() {
                            return Some(StreamEvent {
                                event_type: EventType::Thinking(thinking.to_string()),
                                timestamp: timestamp.clone(),
                            });
                        }
                    }
                    "tool_use" => {
                        let name = block
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let input = block.get("input").cloned().unwrap_or(Value::Null);
                        let input_str = serde_json::to_string(&input).unwrap_or_default();
                        let input_summary = format::format_tool_input(name, &input_str, 120);
                        return Some(StreamEvent {
                            event_type: EventType::ToolUse {
                                name: name.to_string(),
                                input_summary,
                            },
                            timestamp: timestamp.clone(),
                        });
                    }
                    _ => {}
                }
            }
            None
        }
        Some("user") => {
            let message = val.get("message")?;
            let content = message.get("content")?;
            let blocks = content.as_array()?;

            for block in blocks {
                let block_type = block.get("type").and_then(|v| v.as_str())?;
                match block_type {
                    "tool_result" => {
                        let is_error = block
                            .get("is_error")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let content_text =
                            block.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        return Some(StreamEvent {
                            event_type: EventType::ToolResult {
                                content: content_text.to_string(),
                                is_error,
                            },
                            timestamp: timestamp.clone(),
                        });
                    }
                    "text" => {
                        let text = block.get("text").and_then(|v| v.as_str())?;
                        if !text.trim().is_empty() {
                            return Some(StreamEvent {
                                event_type: EventType::UserText(text.to_string()),
                                timestamp: timestamp.clone(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        _ => None,
    }
}

fn parse_pi_event(line: &str) -> Option<StreamEvent> {
    let val: Value = serde_json::from_str(line).ok()?;
    if val.get("type").and_then(|v| v.as_str()) != Some("message") {
        return None;
    }

    let message = val.get("message")?;
    let timestamp = val
        .get("timestamp")
        .and_then(|v| v.as_str())
        .or_else(|| message.get("timestamp").and_then(|v| v.as_str()))
        .map(|s| s.to_string());

    match message.get("role").and_then(|v| v.as_str()) {
        Some("assistant") => {
            let blocks = message.get("content")?.as_array()?;
            for block in blocks {
                match block.get("type").and_then(|v| v.as_str())? {
                    "text" => {
                        let text = block.get("text").and_then(|v| v.as_str())?;
                        if !text.trim().is_empty() {
                            return Some(StreamEvent {
                                event_type: EventType::Text(text.to_string()),
                                timestamp: timestamp.clone(),
                            });
                        }
                    }
                    "thinking" => {
                        let thinking = block
                            .get("thinking")
                            .or_else(|| block.get("text"))
                            .and_then(|v| v.as_str())?;
                        if !thinking.trim().is_empty() {
                            return Some(StreamEvent {
                                event_type: EventType::Thinking(thinking.to_string()),
                                timestamp: timestamp.clone(),
                            });
                        }
                    }
                    "toolCall" => {
                        let name = block
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let args = block.get("arguments").cloned().unwrap_or(Value::Null);
                        let canonical = AgentBackend::Pi.canonicalize_tool_call(name, args).ok()?;
                        let input_summary = format::format_tool_input(
                            &canonical.tool_name,
                            &canonical.tool_input,
                            120,
                        );
                        return Some(StreamEvent {
                            event_type: EventType::ToolUse {
                                name: canonical.tool_name,
                                input_summary,
                            },
                            timestamp: timestamp.clone(),
                        });
                    }
                    _ => {}
                }
            }
            None
        }
        Some("user") => {
            let content = message.get("content")?.as_array()?;
            let text = content
                .iter()
                .filter_map(|block| match block.get("type").and_then(|v| v.as_str()) {
                    Some("text") => block.get("text").and_then(|v| v.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if text.trim().is_empty() {
                None
            } else {
                Some(StreamEvent {
                    event_type: EventType::UserText(text),
                    timestamp,
                })
            }
        }
        Some("toolResult") => {
            let is_error = message
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let content = message.get("content")?.as_array()?;
            let text = content
                .iter()
                .filter_map(|block| block.get("text").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            let preview = if text.len() > 200 {
                format!("{}...", &text[..197])
            } else {
                text
            };
            Some(StreamEvent {
                event_type: EventType::ToolResult {
                    content: preview,
                    is_error,
                },
                timestamp,
            })
        }
        _ => None,
    }
}

/// Print a formatted event to stdout.
fn print_event(color: &str, session_name: &str, event: &StreamEvent, show_session: bool) {
    let ts = event
        .timestamp
        .as_deref()
        .and_then(format_time_short)
        .unwrap_or_default();

    let prefix = if show_session {
        format!("{DIM}{ts}{RESET} {color}{BOLD}{session_name}{RESET}")
    } else {
        format!("{DIM}{ts}{RESET}")
    };

    match &event.event_type {
        EventType::Text(text) => {
            print_block_event(&prefix, "text", text, None);
        }
        EventType::Thinking(thinking) => {
            print_block_event(&prefix, "thinking", thinking, None);
        }
        EventType::UserText(text) => {
            print_block_event(&prefix, "user", text, None);
        }
        EventType::ToolUse {
            name,
            input_summary,
        } => {
            println!("{prefix} {BOLD}{name}{RESET} {input_summary}");
        }
        EventType::ToolResult { content, is_error } => {
            let color = if *is_error { Some("\x1b[31m") } else { None };
            print_block_event(&prefix, "result", content, color);
        }
    }
}

fn print_block_event(prefix: &str, label: &str, body: &str, body_color: Option<&str>) {
    println!("{prefix} {DIM}{label}:{RESET}");
    for line in format_block_lines(body) {
        match body_color {
            Some(color) => println!("  {color}{line}{RESET}"),
            None => println!("  {line}"),
        }
    }
}

fn format_block_lines(body: &str) -> Vec<String> {
    if body.is_empty() {
        return vec![String::new()];
    }
    body.lines().map(|line| line.to_string()).collect()
}

/// Print an event in JSON Lines format.
fn print_json_event(session_name: &str, event: &StreamEvent) {
    let (event_type, content) = match &event.event_type {
        EventType::Text(t) => ("text", t.clone()),
        EventType::Thinking(t) => ("thinking", t.clone()),
        EventType::UserText(t) => ("user", t.clone()),
        EventType::ToolUse {
            name,
            input_summary,
        } => ("tool_use", format!("{name}: {input_summary}")),
        EventType::ToolResult { content, is_error } => {
            let label = if *is_error {
                "tool_error"
            } else {
                "tool_result"
            };
            (label, content.clone())
        }
    };

    let obj = serde_json::json!({
        "session": session_name,
        "type": event_type,
        "content": content,
        "timestamp": event.timestamp,
    });
    println!("{}", serde_json::to_string(&obj).unwrap());
}

/// Format an ISO timestamp to just HH:MM:SS.
fn format_time_short(ts: &str) -> Option<String> {
    let t_pos = ts.find('T')?;
    let time_part = &ts[t_pos + 1..];
    let short = if time_part.len() >= 8 {
        &time_part[..8]
    } else {
        time_part
    };
    Some(short.to_string())
}

/// Find the JSONL file for a given Claude session ID.
fn find_claude_jsonl_for_session(session_id: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let projects_dir = PathBuf::from(home).join(".claude").join("projects");
    if !projects_dir.is_dir() {
        return None;
    }

    let filename = format!("{session_id}.jsonl");

    for entry in fs::read_dir(&projects_dir).ok()?.flatten() {
        let project_path = entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let candidate = project_path.join(&filename);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as IoWrite;
    use tempfile::NamedTempFile;

    #[test]
    fn test_format_time_short() {
        assert_eq!(
            format_time_short("2026-03-12T14:20:39.514Z"),
            Some("14:20:39".to_string())
        );
        assert_eq!(format_time_short("bad"), None);
    }

    #[test]
    fn test_parse_event_assistant_text() {
        let line = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-03-12T14:20:39.514Z",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "Hello world"}]
            }
        })
        .to_string();
        let event = parse_event(AgentBackend::Claude, &line).unwrap();
        assert!(matches!(event.event_type, EventType::Text(ref t) if t == "Hello world"));
        assert_eq!(
            event.timestamp,
            Some("2026-03-12T14:20:39.514Z".to_string())
        );
    }

    #[test]
    fn test_parse_event_thinking() {
        let line = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-03-12T14:20:39.514Z",
            "message": {
                "role": "assistant",
                "content": [{"type": "thinking", "thinking": "Let me analyze this..."}]
            }
        })
        .to_string();
        let event = parse_event(AgentBackend::Claude, &line).unwrap();
        assert!(
            matches!(event.event_type, EventType::Thinking(ref t) if t == "Let me analyze this...")
        );
    }

    #[test]
    fn test_parse_event_tool_use() {
        let line = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-03-12T14:20:39.514Z",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_123",
                    "name": "Bash",
                    "input": {"command": "cargo test"}
                }]
            }
        })
        .to_string();
        let event = parse_event(AgentBackend::Claude, &line).unwrap();
        assert!(matches!(event.event_type, EventType::ToolUse { ref name, .. } if name == "Bash"));
    }

    #[test]
    fn test_parse_event_tool_result() {
        let line = serde_json::json!({
            "type": "user",
            "timestamp": "2026-03-12T14:20:40.000Z",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_123",
                    "content": "test result: all 5 passed"
                }]
            }
        })
        .to_string();
        let event = parse_event(AgentBackend::Claude, &line).unwrap();
        assert!(
            matches!(event.event_type, EventType::ToolResult { ref content, .. } if content.contains("passed"))
        );
    }

    #[test]
    fn test_parse_event_tool_result_error() {
        let line = serde_json::json!({
            "type": "user",
            "timestamp": "2026-03-12T14:20:40.000Z",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_123",
                    "content": "File not found",
                    "is_error": true
                }]
            }
        })
        .to_string();
        let event = parse_event(AgentBackend::Claude, &line).unwrap();
        assert!(matches!(
            event.event_type,
            EventType::ToolResult { is_error: true, .. }
        ));
    }

    #[test]
    fn test_parse_event_ignores_queue_operation() {
        let line = serde_json::json!({
            "type": "queue-operation",
            "operation": "enqueue",
        })
        .to_string();
        assert!(parse_event(AgentBackend::Claude, &line).is_none());
    }

    #[test]
    fn test_parse_event_user_prompt() {
        let line = serde_json::json!({
            "type": "user",
            "timestamp": "2026-03-12T14:20:38.000Z",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": "How do I do X?"}]
            }
        })
        .to_string();
        let event = parse_event(AgentBackend::Claude, &line).unwrap();
        assert!(matches!(event.event_type, EventType::UserText(ref t) if t == "How do I do X?"));
        assert_eq!(
            event.timestamp,
            Some("2026-03-12T14:20:38.000Z".to_string())
        );
    }

    #[test]
    fn test_read_new_lines() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "line1\nline2\n").unwrap();
        file.flush().unwrap();

        let mut state = TailState {
            backend: AgentBackend::Claude,
            path: file.path().to_path_buf(),
            offset: 0,
            session_name: "test".to_string(),
            color: COLORS[0],
        };

        let lines = read_new_lines(&mut state);
        assert_eq!(lines, vec!["line1", "line2"]);

        let lines = read_new_lines(&mut state);
        assert!(lines.is_empty());

        writeln!(file, "line3").unwrap();
        file.flush().unwrap();
        let lines = read_new_lines(&mut state);
        assert_eq!(lines, vec!["line3"]);
    }

    #[test]
    fn test_read_last_n_events() {
        let mut file = NamedTempFile::new().unwrap();
        for i in 1..=10 {
            let line = serde_json::json!({
                "type": "assistant",
                "timestamp": format!("2026-03-12T14:20:{:02}.000Z", i),
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": format!("message {i}")}]
                }
            });
            writeln!(file, "{}", serde_json::to_string(&line).unwrap()).unwrap();
        }
        file.flush().unwrap();

        let events =
            read_last_n_events(AgentBackend::Claude, &file.path().to_path_buf(), 3).unwrap();
        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0].event_type, EventType::Text(t) if t == "message 8"));
        assert!(matches!(&events[2].event_type, EventType::Text(t) if t == "message 10"));
    }

    #[test]
    fn test_parse_pi_event_tool_call() {
        let line = serde_json::json!({
            "type": "message",
            "timestamp": "2026-03-12T14:20:39.514Z",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "toolCall",
                    "name": "read",
                    "arguments": {"path": "src/main.rs"}
                }]
            }
        })
        .to_string();
        let event = parse_event(AgentBackend::Pi, &line).unwrap();
        assert!(matches!(event.event_type, EventType::ToolUse { ref name, .. } if name == "Read"));
    }

    #[test]
    fn test_parse_pi_event_user_text() {
        let line = serde_json::json!({
            "type": "message",
            "timestamp": "2026-03-12T14:20:38.000Z",
            "message": {
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": "Please fix the tests"
                }]
            }
        })
        .to_string();
        let event = parse_event(AgentBackend::Pi, &line).unwrap();
        assert!(
            matches!(event.event_type, EventType::UserText(ref content) if content == "Please fix the tests")
        );
    }

    #[test]
    fn test_parse_pi_event_tool_result() {
        let line = serde_json::json!({
            "type": "message",
            "timestamp": "2026-03-12T14:20:40.000Z",
            "message": {
                "role": "toolResult",
                "toolName": "read",
                "isError": true,
                "content": [{
                    "type": "text",
                    "text": "ENOENT: no such file"
                }]
            }
        })
        .to_string();
        let event = parse_event(AgentBackend::Pi, &line).unwrap();
        assert!(matches!(
            event.event_type,
            EventType::ToolResult { is_error: true, ref content }
                if content.contains("ENOENT")
        ));
    }

    #[test]
    fn test_find_session_path_for_pi_uses_agent_session_file() {
        let file = NamedTempFile::new().unwrap();
        let session = crate::db::Session {
            _id: 1,
            session_id: "cq-session".into(),
            agent_backend: AgentBackend::Pi,
            agent_session_id: Some(file.path().to_string_lossy().into_owned()),
            claude_session_id: None,
            name: Some("pi-task".into()),
            prompt: "prompt".into(),
            _cwd: ".".into(),
            status: "running".into(),
            pid: None,
            started_at: "2026-03-12T14:20:39.514Z".into(),
            _completed_at: None,
            _exit_code: None,
        };

        let path = find_session_path(&session).unwrap();
        assert_eq!(path, file.path());
    }

    #[test]
    fn test_find_sessions_with_name_filter_returns_all_named_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cq.db")).unwrap();

        let first = NamedTempFile::new().unwrap();
        let second = NamedTempFile::new().unwrap();
        db.create_session(
            "s1",
            AgentBackend::Pi,
            Some(first.path().to_string_lossy().as_ref()),
            Some("same"),
            "prompt 1",
            ".",
            Some(1),
        )
        .unwrap();
        db.create_session(
            "s2",
            AgentBackend::Pi,
            Some(second.path().to_string_lossy().as_ref()),
            Some("same"),
            "prompt 2",
            ".",
            Some(2),
        )
        .unwrap();

        let sessions = find_sessions(&db, Some("same")).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].path, first.path());
        assert_eq!(sessions[1].path, second.path());
        assert!(sessions.iter().all(|session| session.name == "same"));
    }

    #[test]
    fn test_format_block_lines_preserves_paragraphs_without_pipes() {
        let lines = format_block_lines(
            "Yes. I ran `pwd` successfully and got:\n\n`/Users/wtj/src/github.com/wiggzz/claude-queue`",
        );
        assert_eq!(
            lines,
            vec![
                "Yes. I ran `pwd` successfully and got:".to_string(),
                "".to_string(),
                "`/Users/wtj/src/github.com/wiggzz/claude-queue`".to_string(),
            ]
        );
    }
}
