use crate::config;
use crate::db::Db;
use std::fs;
use std::process::{Command, Stdio};

/// Start a brand new sub-agent session.
pub fn start(prompt: &str, name: Option<&str>, cwd: &str) -> Result<String, Box<dyn std::error::Error>> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let args = vec![
        "-p".to_string(),
        "--session-id".to_string(), session_id.clone(),
        prompt.to_string(),
    ];
    launch(&session_id, Some(&session_id), name, prompt, cwd, args)
}

/// Resume a session. Accepts either a cq session ID prefix or a raw claude session ID.
/// Looks up the claude_session_id from the DB if it's a cq prefix, otherwise uses it directly.
pub fn resume(id_or_prefix: &str, prompt: &str, cwd: &str) -> Result<String, Box<dyn std::error::Error>> {
    let db = Db::open(&config::db_path())?;

    // Try to find by cq session prefix first, then by claude session ID
    let claude_sid = if let Some(sess) = db.find_session(id_or_prefix)? {
        // Found a cq session — use its claude_session_id, falling back to session_id
        sess.claude_session_id.unwrap_or(sess.session_id)
    } else {
        // Not in our DB — treat as a raw claude session ID
        id_or_prefix.to_string()
    };

    let cq_session_id = uuid::Uuid::new_v4().to_string();
    let args = vec![
        "-p".to_string(),
        "-r".to_string(), claude_sid.clone(),
        prompt.to_string(),
    ];
    // Inherit the name from the original session if it had one
    let name = if let Some(sess) = db.find_session(id_or_prefix)? {
        sess.name
    } else {
        None
    };

    let display_prompt = format!("[resumed {}] {}", &claude_sid[..8.min(claude_sid.len())], prompt);
    launch(&cq_session_id, Some(&claude_sid), name.as_deref(), &display_prompt, cwd, args)
}

/// Common launch logic for both start and resume.
fn launch(session_id: &str, claude_session_id: Option<&str>, name: Option<&str>, prompt_display: &str, cwd: &str, extra_args: Vec<String>) -> Result<String, Box<dyn std::error::Error>> {
    let cwd_abs = fs::canonicalize(cwd)?;
    let db_path = config::db_path();
    let log_dir = config::log_dir();
    fs::create_dir_all(&log_dir)?;

    let log_path = log_dir.join(format!("{session_id}.log"));
    let log_file = fs::File::create(&log_path)?;
    let stderr_path = log_dir.join(format!("{session_id}.stderr"));
    let stderr_file = fs::File::create(&stderr_path)?;

    // Build the hook settings JSON
    let cq_bin = std::env::current_exe()
        .unwrap_or_else(|_| "cq".into());
    let hook_command = format!("{} hook", cq_bin.display());
    let settings = serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": hook_command,
                    "timeout": 90000
                }]
            }]
        }
    });
    let settings_str = serde_json::to_string(&settings)?;

    let mut cmd = Command::new("claude");
    for arg in &extra_args {
        cmd.arg(arg);
    }
    let child = cmd
        .args([
            "--settings", &settings_str,
            "--permission-mode", "bypassPermissions",
            "--dangerously-skip-permissions",
        ])
        .env("CQ_MANAGED", "1")
        .env("CQ_DB", db_path.to_string_lossy().as_ref())
        .env_remove("CLAUDECODE")
        .current_dir(&cwd_abs)
        .stdout(log_file)
        .stderr(stderr_file)
        .spawn()?;

    let pid = child.id();

    // Record in DB
    let db = Db::open(&db_path)?;
    db.create_session(session_id, claude_session_id, name, prompt_display, &cwd_abs.to_string_lossy(), pid)?;

    // Spawn a thread to wait for completion and update DB
    let sid = session_id.to_string();
    let dbp = db_path.clone();
    std::thread::spawn(move || {
        let mut child = child;
        let status = child.wait();
        if let Ok(db) = Db::open(&dbp) {
            match status {
                Ok(s) => {
                    let code = s.code();
                    let st = if code == Some(0) { "completed" } else { "failed" };
                    let _ = db.update_session_status(&sid, st, code);
                }
                Err(_) => {
                    let _ = db.update_session_status(&sid, "failed", None);
                }
            }
        }
    });

    Ok(session_id.to_string())
}

/// Check the log file for a dead session and update DB status accordingly.
pub fn resolve_dead_session(db: &crate::db::Db, session_id: &str) -> String {
    let log_path = config::log_dir().join(format!("{session_id}.log"));
    let stderr_path = config::log_dir().join(format!("{session_id}.stderr"));
    let status = if let Ok(content) = fs::read_to_string(&log_path) {
        if !content.trim().is_empty() {
            let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
            if stderr.contains("Error:") { "failed" } else { "completed" }
        } else {
            let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
            if stderr.trim().is_empty() { "completed" } else { "failed" }
        }
    } else {
        "failed"
    };
    let _ = db.update_session_status(session_id, status, None);
    status.to_string()
}

pub fn get_output(session_id: &str) -> Result<String, Box<dyn std::error::Error>> {
    let log_path = config::log_dir().join(format!("{session_id}.log"));
    if !log_path.exists() {
        return Err(format!("No log file found for session {session_id}").into());
    }
    Ok(fs::read_to_string(&log_path)?)
}

pub fn get_stderr(session_id: &str) -> Result<String, Box<dyn std::error::Error>> {
    let path = config::log_dir().join(format!("{session_id}.stderr"));
    if !path.exists() {
        return Err(format!("No stderr file found for session {session_id}").into());
    }
    Ok(fs::read_to_string(&path)?)
}

pub fn kill_session(pid: i64) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    {
        Command::new("kill").arg(pid.to_string()).status()?;
    }
    Ok(())
}

pub fn is_pid_alive(pid: i64) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}
