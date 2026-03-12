use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::config;
use crate::db::Db;

/// Metadata extracted from a non-cq-managed Claude Code session.
#[derive(Debug)]
pub struct DiscoveredSession {
    pub session_id: String,
    pub project_dir: String,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub first_prompt: Option<String>,
    pub last_activity: Option<String>,
    pub message_count: usize,
    pub jsonl_path: PathBuf,
}

/// Get session IDs managed by cq so we can filter them out.
fn get_cq_managed_ids() -> HashSet<String> {
    let mut ids = HashSet::new();
    if let Ok(db) = Db::open(&config::db_path())
        && let Ok(sessions) = db.get_sessions()
    {
        for s in sessions {
            if let Some(claude_id) = s.claude_session_id {
                ids.insert(claude_id);
            }
        }
    }
    ids
}

/// Find the Claude projects directory (~/.claude/projects/).
fn claude_projects_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".claude").join("projects")
}

/// Scan all JSONL files under ~/.claude/projects/ and return discovered sessions.
pub fn scan_sessions() -> Vec<DiscoveredSession> {
    let managed_ids = get_cq_managed_ids();
    let projects_dir = claude_projects_dir();

    if !projects_dir.is_dir() {
        return Vec::new();
    }

    let mut sessions = Vec::new();

    // Walk ~/.claude/projects/<project-dir>/*.jsonl
    let project_dirs = match fs::read_dir(&projects_dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    for project_entry in project_dirs.flatten() {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let project_name = project_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let jsonl_files = match fs::read_dir(&project_path) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for file_entry in jsonl_files.flatten() {
            let file_path = file_entry.path();
            if file_path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }

            if let Some(session) = parse_session_file(&file_path, &project_name, &managed_ids) {
                sessions.push(session);
            }
        }
    }

    // Sort by last activity, most recent first
    sessions.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    sessions
}

/// Parse metadata from a session JSONL file without loading it entirely into memory.
fn parse_session_file(
    path: &Path,
    project_name: &str,
    managed_ids: &HashSet<String>,
) -> Option<DiscoveredSession> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut session_id: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut git_branch: Option<String> = None;
    let mut first_prompt: Option<String> = None;
    let mut last_activity: Option<String> = None;
    let mut message_count: usize = 0;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }

        let val: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Extract session ID
        if session_id.is_none()
            && let Some(id) = val.get("sessionId").and_then(|v| v.as_str())
        {
            if managed_ids.contains(id) {
                return None; // This is a cq-managed session, skip it
            }
            session_id = Some(id.to_string());
        }

        // Extract cwd
        if cwd.is_none()
            && let Some(c) = val.get("cwd").and_then(|v| v.as_str())
        {
            cwd = Some(c.to_string());
        }

        // Extract git branch
        if git_branch.is_none()
            && let Some(branch) = val.get("gitBranch").and_then(|v| v.as_str())
        {
            git_branch = Some(branch.to_string());
        }

        // Extract first user prompt
        if first_prompt.is_none() {
            let msg_type = val.get("type").and_then(|v| v.as_str());
            if msg_type == Some("user") {
                first_prompt = extract_message_text(&val);
            }
        }

        // Track timestamp (last one wins)
        if let Some(ts) = val.get("timestamp").and_then(|v| v.as_str()) {
            last_activity = Some(ts.to_string());
        }

        message_count += 1;
    }

    let session_id = session_id?;

    Some(DiscoveredSession {
        session_id,
        project_dir: project_name.to_string(),
        cwd,
        git_branch,
        first_prompt,
        last_activity,
        message_count,
        jsonl_path: path.to_path_buf(),
    })
}

/// Extract text content from a message's content field.
pub(crate) fn extract_message_text(val: &Value) -> Option<String> {
    let message = val.get("message")?;
    let content = message.get("content")?;

    // content can be a string or an array of content blocks
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }

    if let Some(arr) = content.as_array() {
        let mut parts = Vec::new();
        for block in arr {
            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                parts.push(text.to_string());
            }
        }
        if !parts.is_empty() {
            return Some(parts.join(" "));
        }
    }

    None
}

/// Search through session JSONL files for a query string.
pub fn search_sessions(query: &str) -> Vec<DiscoveredSession> {
    let managed_ids = get_cq_managed_ids();
    let projects_dir = claude_projects_dir();

    if !projects_dir.is_dir() {
        return Vec::new();
    }

    let query_lower = query.to_lowercase();
    let mut results = Vec::new();

    let project_dirs = match fs::read_dir(&projects_dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    for project_entry in project_dirs.flatten() {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let project_name = project_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let jsonl_files = match fs::read_dir(&project_path) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for file_entry in jsonl_files.flatten() {
            let file_path = file_entry.path();
            if file_path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }

            if let Some(session) =
                search_session_file(&file_path, &project_name, &managed_ids, &query_lower)
            {
                results.push(session);
            }
        }
    }

    results.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    results
}

/// Search a single JSONL file for a query string, returning metadata if found.
fn search_session_file(
    path: &Path,
    project_name: &str,
    managed_ids: &HashSet<String>,
    query_lower: &str,
) -> Option<DiscoveredSession> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut session_id: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut git_branch: Option<String> = None;
    let mut first_prompt: Option<String> = None;
    let mut last_activity: Option<String> = None;
    let mut message_count: usize = 0;
    let mut found = false;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }

        // Quick check: does this line contain the query at all?
        if !found && line.to_lowercase().contains(query_lower) {
            found = true;
        }

        let val: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if session_id.is_none()
            && let Some(id) = val.get("sessionId").and_then(|v| v.as_str())
        {
            if managed_ids.contains(id) {
                return None;
            }
            session_id = Some(id.to_string());
        }

        if cwd.is_none()
            && let Some(c) = val.get("cwd").and_then(|v| v.as_str())
        {
            cwd = Some(c.to_string());
        }

        if git_branch.is_none()
            && let Some(branch) = val.get("gitBranch").and_then(|v| v.as_str())
        {
            git_branch = Some(branch.to_string());
        }

        if first_prompt.is_none() {
            let msg_type = val.get("type").and_then(|v| v.as_str());
            if msg_type == Some("user") {
                first_prompt = extract_message_text(&val);
            }
        }

        if let Some(ts) = val.get("timestamp").and_then(|v| v.as_str()) {
            last_activity = Some(ts.to_string());
        }

        message_count += 1;
    }

    if !found {
        return None;
    }

    let session_id = session_id?;

    Some(DiscoveredSession {
        session_id,
        project_dir: project_name.to_string(),
        cwd,
        git_branch,
        first_prompt,
        last_activity,
        message_count,
        jsonl_path: path.to_path_buf(),
    })
}

/// Find a specific session by ID (prefix match).
pub fn find_session(query: &str) -> Option<DiscoveredSession> {
    let sessions = scan_sessions();
    sessions
        .into_iter()
        .find(|s| s.session_id.starts_with(query))
}

/// Get a summary of recent activity from a session JSONL file.
pub fn get_session_summary(path: &Path, max_messages: usize) -> Vec<String> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut messages = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }

        let val: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let msg_type = val
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        match msg_type {
            "user" => {
                if let Some(text) = extract_message_text(&val) {
                    let truncated = if text.len() > 200 {
                        format!("{}...", &text[..197])
                    } else {
                        text
                    };
                    messages.push(format!("[user] {truncated}"));
                }
            }
            "assistant" => {
                if let Some(text) = extract_message_text(&val) {
                    let truncated = if text.len() > 200 {
                        format!("{}...", &text[..197])
                    } else {
                        text
                    };
                    messages.push(format!("[assistant] {truncated}"));
                }
            }
            _ => {}
        }
    }

    // Return last N messages
    let start = messages.len().saturating_sub(max_messages);
    messages[start..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_extract_message_text_string() {
        let val: Value = serde_json::json!({
            "type": "user",
            "message": {
                "content": "hello world"
            }
        });
        assert_eq!(extract_message_text(&val), Some("hello world".to_string()));
    }

    #[test]
    fn test_extract_message_text_array() {
        let val: Value = serde_json::json!({
            "type": "user",
            "message": {
                "content": [
                    {"type": "text", "text": "hello"},
                    {"type": "text", "text": "world"}
                ]
            }
        });
        assert_eq!(extract_message_text(&val), Some("hello world".to_string()));
    }

    #[test]
    fn test_extract_message_text_empty_array() {
        let val: Value = serde_json::json!({
            "type": "user",
            "message": {
                "content": []
            }
        });
        assert_eq!(extract_message_text(&val), None);
    }

    #[test]
    fn test_extract_message_text_no_message() {
        let val: Value = serde_json::json!({
            "type": "user",
            "sessionId": "abc123"
        });
        assert_eq!(extract_message_text(&val), None);
    }

    #[test]
    fn test_get_session_summary_empty_file() {
        let file = NamedTempFile::new().unwrap();
        let result = get_session_summary(file.path(), 10);
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_session_summary_messages() {
        let mut file = NamedTempFile::new().unwrap();
        let lines = [
            serde_json::json!({"type": "user", "message": {"content": "What is Rust?"}}),
            serde_json::json!({"type": "assistant", "message": {"content": "A systems language."}}),
            serde_json::json!({"type": "user", "message": {"content": "Tell me more."}}),
        ];
        for line in &lines {
            writeln!(file, "{}", line).unwrap();
        }
        file.flush().unwrap();

        let result = get_session_summary(file.path(), 10);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], "[user] What is Rust?");
        assert_eq!(result[1], "[assistant] A systems language.");
        assert_eq!(result[2], "[user] Tell me more.");
    }

    #[test]
    fn test_get_session_summary_max_messages() {
        let mut file = NamedTempFile::new().unwrap();
        for i in 0..10 {
            let msg = serde_json::json!({
                "type": "user",
                "message": {"content": format!("message {i}")}
            });
            writeln!(file, "{}", msg).unwrap();
        }
        file.flush().unwrap();

        let result = get_session_summary(file.path(), 3);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], "[user] message 7");
        assert_eq!(result[1], "[user] message 8");
        assert_eq!(result[2], "[user] message 9");
    }
}
