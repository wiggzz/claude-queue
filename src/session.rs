use crate::config;
use crate::db::Db;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Result of a push operation.
pub enum PushResult {
    /// A new session was started (session_id).
    Started(String),
    /// The message was queued for delivery when the running session completes.
    Queued,
    /// The session was resumed with the message (session_id).
    Resumed(String),
}

/// Push a message to a named session. This is the primary entry point:
/// - No session exists → start a new one
/// - Session is running → queue the message
/// - Session is completed/failed → resume it (with any previously queued messages)
pub fn push(prompt: &str, name: &str, cwd: &str) -> Result<PushResult, Box<dyn std::error::Error>> {
    let db = Db::open(&config::db_path())?;

    if let Some(sess) = db.find_session(name)? {
        // Warn if the provided cwd differs from the session's original cwd.
        // Claude stores sessions per-project, so resume always uses the original cwd.
        if let Ok(cwd_abs) = fs::canonicalize(cwd) {
            if cwd_abs.to_string_lossy() != sess._cwd {
                eprintln!(
                    "warning: --cwd '{}' differs from session's original cwd '{}'. Resume will use the original cwd.",
                    cwd_abs.display(),
                    sess._cwd,
                );
            }
        }

        let alive = sess.pid.map(is_pid_alive).unwrap_or(false);
        if (sess.status == "running" && alive) || sess.status == "delivering" {
            // Session is running or wrapper is delivering queued messages — queue this message.
            // The wrapper will deliver it after the current session/delivery completes.
            let cwd_abs = fs::canonicalize(cwd)?;
            db.push_queued_message(name, prompt, Some(&cwd_abs.to_string_lossy()))?;
            return Ok(PushResult::Queued);
        }
        // Session exists but is done — claim it for delivery to prevent race with wrapper
        if !db.claim_session_for_delivery(&sess.session_id)? {
            // Wrapper already claimed — queue this message, it will be delivered
            let cwd_abs = fs::canonicalize(cwd)?;
            db.push_queued_message(name, prompt, Some(&cwd_abs.to_string_lossy()))?;
            return Ok(PushResult::Queued);
        }
        // We won the claim — collect any queued messages + this one, resume
        let mut messages = db.take_all_queued_messages(name)?;
        messages.push((prompt.to_string(), Some(cwd.to_string())));
        let combined_prompt = messages
            .iter()
            .map(|(p, _)| p.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        // Use the last non-None cwd, falling back to the provided cwd
        let resume_cwd = messages
            .iter()
            .rev()
            .find_map(|(_, c)| c.as_deref())
            .unwrap_or(cwd);
        let session_id = resume_session(name, &sess, &combined_prompt, resume_cwd)?;
        // Reset the old session's status back from "delivering"
        let _ = db.update_session_status(&sess.session_id, &sess.status, sess._exit_code);
        return Ok(PushResult::Resumed(session_id));
    }

    // No session exists — start a new one
    let session_id = start(prompt, Some(name), cwd)?;
    Ok(PushResult::Started(session_id))
}

/// Interrupt a running session: kill it, clear the queue, and resume with a new message.
pub fn interrupt(
    name: &str,
    prompt: &str,
    cwd: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let db = Db::open(&config::db_path())?;
    let sess = db
        .find_session(name)?
        .ok_or_else(|| format!("No session found: {name}"))?;

    // Kill the running session if alive
    if let Some(pid) = sess.pid
        && pid > 0
        && is_pid_alive(pid)
    {
        kill_session(pid)?;
        // Wait briefly for process to die
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let _ = db.update_session_status(&sess.session_id, "killed", None);

    // Clear the queue
    let cleared = db.clear_queued_messages(name)?;
    if cleared > 0 {
        eprintln!("Cleared {cleared} queued message(s) for session {name}.");
    }

    // Resume with the interrupt message
    let session_id = resume_session(name, &sess, prompt, cwd)?;
    Ok(session_id)
}

/// Cancel all queued messages for a named session.
pub fn cancel_queued(name: &str) -> Result<usize, Box<dyn std::error::Error>> {
    let db = Db::open(&config::db_path())?;
    Ok(db.clear_queued_messages(name)?)
}

/// Start a brand new sub-agent session.
/// Fails if a named session already exists and is still running (use `cq push` instead).
pub fn start(
    prompt: &str,
    name: Option<&str>,
    cwd: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // Guard: if a named session is already running, refuse to start a duplicate
    if let Some(name) = name {
        let db = Db::open(&config::db_path())?;
        if let Some(sess) = db.find_session(name)? {
            let alive = sess.pid.map(is_pid_alive).unwrap_or(false);
            if sess.status == "running" && alive {
                return Err(format!(
                    "Session '{name}' is already running. Use `cq push {name} \"...\"` to queue a message, or `cq interrupt {name} \"...\"` to kill and restart."
                ).into());
            }
        }
    }

    let session_id = uuid::Uuid::new_v4().to_string();
    let args = vec![
        "-p".to_string(),
        "--session-id".to_string(),
        session_id.clone(),
        prompt.to_string(),
    ];
    launch(&session_id, Some(&session_id), name, prompt, cwd, args)
}

/// Resume a session by name, using the claude_session_id from a previous session.
/// Always resumes from the previous session's cwd to maintain the same claude project context.
/// The `cwd` parameter is ignored — we use prev_session's stored cwd instead.
fn resume_session(
    name: &str,
    prev_session: &crate::db::Session,
    prompt: &str,
    _cwd: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let cwd = &prev_session._cwd;
    let claude_sid = prev_session
        .claude_session_id
        .clone()
        .unwrap_or_else(|| prev_session.session_id.clone());

    let cq_session_id = uuid::Uuid::new_v4().to_string();
    let args = vec![
        "-p".to_string(),
        "--resume".to_string(),
        claude_sid.clone(),
        prompt.to_string(),
    ];

    let display_prompt = format!(
        "[resumed {}] {}",
        &claude_sid[..8.min(claude_sid.len())],
        prompt
    );
    launch(
        &cq_session_id,
        Some(&claude_sid),
        Some(name),
        &display_prompt,
        cwd,
        args,
    )
}

/// Common launch logic for both start and resume.
/// Pre-registers the session in DB, then spawns a background `cq run-session` process
/// which is the direct parent of claude (so it can use proper child.wait()).
fn launch(
    session_id: &str,
    claude_session_id: Option<&str>,
    name: Option<&str>,
    prompt_display: &str,
    cwd: &str,
    claude_args: Vec<String>,
) -> Result<String, Box<dyn std::error::Error>> {
    let cwd_abs = fs::canonicalize(cwd)?;
    let db_path = config::db_path();

    // Pre-register session in DB so it shows up in `cq list` immediately
    let db = Db::open(&db_path)?;
    db.create_session(
        session_id,
        claude_session_id,
        name,
        prompt_display,
        &cwd_abs.to_string_lossy(),
        None, // PID will be updated by run-session once claude is spawned
    )?;

    // Spawn the wrapper process that will be claude's direct parent
    let cq_bin = std::env::current_exe().unwrap_or_else(|_| "cq".into());
    let mut cmd = Command::new(&cq_bin);
    cmd.args([
        "run-session",
        session_id,
        "--cwd",
        &cwd_abs.to_string_lossy(),
        "--prompt-display",
        prompt_display,
    ]);
    if let Some(csid) = claude_session_id {
        cmd.args(["--claude-session-id", csid]);
    }
    if let Some(name) = name {
        cmd.args(["--name", name]);
    }
    cmd.arg("--");
    cmd.args(&claude_args);

    let log_dir = config::log_dir();
    fs::create_dir_all(&log_dir)?;
    let wrapper_stderr_path = log_dir.join(format!("{session_id}.wrapper.stderr"));
    let wrapper_stderr_file = fs::File::create(&wrapper_stderr_path)?;

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(wrapper_stderr_file)
        .spawn()?;

    // Wait for the wrapper to spawn claude and set the PID before returning.
    // This ensures that by the time push/start returns, the session is fully set up
    // and interrupt/kill can work immediately without polling.
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let check_db = Db::open(&db_path)?;
        if let Some(sess) = check_db.find_session(session_id)?
            && sess.pid.is_some()
        {
            return Ok(session_id.to_string());
        }
    }

    // Wrapper didn't set PID within 5s — return anyway, something may be wrong
    Ok(session_id.to_string())
}

/// Wait for a child process to exit. Returns (status_str, exit_code) without updating DB.
fn wait_for_child(
    session_id: &str,
    child: &mut std::process::Child,
) -> Result<(&'static str, Option<i32>), Box<dyn std::error::Error>> {
    let status = child.wait();
    match status {
        Ok(s) => {
            let code = s.code();
            eprintln!(
                "[run-session {session_id}] claude exited: code={code:?}, success={}",
                s.success()
            );
            let st = if code == Some(0) {
                "completed"
            } else {
                "failed"
            };
            Ok((st, code))
        }
        Err(e) => {
            eprintln!("[run-session {session_id}] wait error: {e}");
            Ok(("failed", None))
        }
    }
}

/// Run a session: spawn claude as a child process, wait for it, update DB, deliver queued messages.
/// This is invoked as `cq run-session` — a background wrapper process that is the direct parent of claude.
pub fn run_session(
    session_id: &str,
    name: Option<&str>,
    cwd: &str,
    prompt_display: &str,
    claude_args: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let cwd_abs = fs::canonicalize(cwd)?;
    let project_root = resolve_project_root(&cwd_abs);
    let db_path = config::db_path();
    let log_dir = config::log_dir();
    fs::create_dir_all(&log_dir)?;

    let log_path = log_dir.join(format!("{session_id}.log"));
    let stderr_path = log_dir.join(format!("{session_id}.stderr"));

    // Build the hook settings JSON
    let cq_bin = std::env::current_exe().unwrap_or_else(|_| "cq".into());
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

    // Load config for model setting
    let config = crate::config::Config::load(&project_root);

    // Shared helper: spawn claude with given args and common env/settings
    let spawn_claude = |args: &[String],
                        log_path: &std::path::Path,
                        stderr_path: &std::path::Path|
     -> Result<std::process::Child, Box<dyn std::error::Error>> {
        let log_file = fs::File::create(log_path)?;
        let stderr_file = fs::File::create(stderr_path)?;
        let mut cmd = Command::new("claude");
        cmd.args(args);
        if !config.model.is_empty() {
            cmd.args(["--model", &config.model]);
        }
        let child = cmd
            .args([
                "--settings",
                &settings_str,
                "--permission-mode",
                "bypassPermissions",
                "--dangerously-skip-permissions",
            ])
            .env("CQ_MANAGED", "1")
            .env("CQ_DB", db_path.to_string_lossy().as_ref())
            .env("CQ_PROJECT_DIR", project_root.to_string_lossy().as_ref())
            .env("CQ_SESSION_CWD", cwd_abs.to_string_lossy().as_ref())
            .env("CQ_SESSION_NAME", name.unwrap_or(""))
            .env("CQ_SESSION_PROMPT", prompt_display)
            .env_remove("CLAUDECODE")
            .current_dir(&cwd_abs)
            .stdout(log_file)
            .stderr(stderr_file)
            .spawn()?;
        Ok(child)
    };

    let mut child = spawn_claude(&claude_args, &log_path, &stderr_path)?;
    let db = Db::open(&db_path)?;
    db.update_session_pid(session_id, child.id())?;

    // Wait for claude to exit (we are its direct parent, so this is proper waitpid)
    let (mut final_status, mut exit_code) = wait_for_child(session_id, &mut child)?;

    // If claude failed because the session doesn't exist (e.g., killed before saving),
    // retry as a fresh session without --resume.
    // Don't update DB status yet — we might retry, and setting "failed" transiently
    // could confuse polling clients.
    if final_status == "failed" && claude_args.contains(&"--resume".to_string()) {
        let stderr_content = fs::read_to_string(&stderr_path).unwrap_or_default();
        if stderr_content.contains("No conversation found")
            || stderr_content.contains("Session not found")
        {
            eprintln!(
                "[run-session {session_id}] Resume failed (session not found), retrying as fresh session..."
            );
            // Build fresh args: remove --resume <id>, keep everything else
            let fresh_args: Vec<String> = {
                let mut out = Vec::new();
                let mut skip_next = false;
                for arg in &claude_args {
                    if skip_next {
                        skip_next = false;
                        continue;
                    }
                    if arg == "--resume" {
                        skip_next = true;
                        continue;
                    }
                    out.push(arg.clone());
                }
                out
            };

            let mut child = spawn_claude(&fresh_args, &log_path, &stderr_path)?;
            db.update_session_pid(session_id, child.id())?;

            let result = wait_for_child(session_id, &mut child)?;
            final_status = result.0;
            exit_code = result.1;
        }
    }

    // Now update the DB with the final status (after any retries)
    let _ = db.update_session_status(session_id, final_status, exit_code);

    // Atomically claim the session for delivery — prevents race with concurrent `cq push`
    if let Some(name) = name {
        if !db.claim_session_for_delivery(session_id)? {
            // Another process (e.g. `cq push`) already claimed this session — let it handle delivery
            return Ok(());
        }
        let messages = db.take_all_queued_messages(name)?;
        if messages.is_empty() {
            // No queued messages — reset status back from "delivering"
            let _ = db.update_session_status(session_id, final_status, exit_code);
        } else {
            let combined_prompt = messages
                .iter()
                .map(|(p, _)| p.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let resume_cwd = messages
                .iter()
                .rev()
                .find_map(|(_, c)| c.as_deref())
                .unwrap_or(cwd);
            eprintln!(
                "Delivering {} queued message(s) for session {name}...",
                messages.len()
            );
            if let Some(sess) = db.find_session(name)? {
                match resume_session(name, &sess, &combined_prompt, resume_cwd) {
                    Ok(new_id) => {
                        // Reset this session's status now that delivery succeeded
                        let _ = db.update_session_status(session_id, final_status, exit_code);
                        eprintln!("Queued messages delivered: session {name} resumed ({new_id})");
                    }
                    Err(e) => {
                        let _ = db.update_session_status(session_id, final_status, exit_code);
                        eprintln!("Failed to deliver queued messages for session {name}: {e}");
                    }
                }
            }
        }
    }

    Ok(())
}

/// Check the log file for a dead session and update DB status accordingly.
pub fn resolve_dead_session(db: &crate::db::Db, session_id: &str) -> String {
    let log_path = config::log_dir().join(format!("{session_id}.log"));
    let stderr_path = config::log_dir().join(format!("{session_id}.stderr"));
    let status = if let Ok(content) = fs::read_to_string(&log_path) {
        if !content.trim().is_empty() {
            let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
            if stderr.contains("Error:") {
                "failed"
            } else {
                "completed"
            }
        } else {
            let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
            if stderr.trim().is_empty() {
                "completed"
            } else {
                "failed"
            }
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
    if pid <= 0 {
        return Err(format!("Invalid PID: {pid}").into());
    }
    #[cfg(unix)]
    unsafe {
        // Use SIGINT for graceful shutdown (like Ctrl-C), not SIGTERM
        libc::kill(pid as i32, libc::SIGINT);
    }
    #[cfg(not(unix))]
    {
        Command::new("kill")
            .args(["-INT", &pid.to_string()])
            .status()?;
    }
    Ok(())
}

/// Resolve the project root directory, handling git worktrees.
/// If `cwd` itself contains `.cq/config.json`, it is used directly (local config wins).
/// Otherwise, for worktrees, returns the main repository's root.
/// Falls back to `git rev-parse --show-toplevel`, then to the given cwd.
pub fn resolve_project_root(cwd: &Path) -> std::path::PathBuf {
    // If cwd has its own .cq/config.json, use it directly — local config takes precedence.
    if cwd.join(".cq").join("config.json").exists() {
        return cwd.to_path_buf();
    }

    // Try git --git-common-dir to find the shared .git directory.
    // For worktrees this points to the main repo's .git dir.
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(cwd)
        .output()
        && output.status.success()
    {
        let common_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let common_path = std::path::Path::new(&common_dir);
        // The common git dir is <repo>/.git — parent is the project root
        if let Some(root) = common_path.parent() {
            return root.to_path_buf();
        }
    }

    // Fallback: git toplevel
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        && output.status.success()
    {
        let toplevel = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !toplevel.is_empty() {
            return std::path::PathBuf::from(toplevel);
        }
    }

    // Not a git repo — use cwd
    cwd.to_path_buf()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_project_root_prefers_local_config() {
        // Create a temp dir with .cq/config.json — should be returned as-is
        let dir = tempfile::tempdir().unwrap();
        let cq_dir = dir.path().join(".cq");
        fs::create_dir_all(&cq_dir).unwrap();
        fs::write(cq_dir.join("config.json"), r#"{"timeout": 100}"#).unwrap();

        let result = resolve_project_root(dir.path());
        assert_eq!(result, dir.path());
    }

    #[test]
    fn test_resolve_project_root_falls_back_to_git_root() {
        // Create a temp git repo without .cq/config.json
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Create a subdirectory to resolve from
        let sub = dir.path().join("subdir");
        fs::create_dir_all(&sub).unwrap();

        let result = resolve_project_root(&sub);
        // Should resolve to the git repo root, not the subdir
        let canonical_dir = fs::canonicalize(dir.path()).unwrap();
        assert_eq!(result, canonical_dir);
    }

    #[test]
    fn test_resolve_project_root_local_config_beats_worktree_parent() {
        // Create a main repo with .cq/config.json
        let main_dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(main_dir.path())
            .output()
            .unwrap();
        // Need an initial commit for worktrees
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(main_dir.path())
            .output()
            .unwrap();
        let main_cq = main_dir.path().join(".cq");
        fs::create_dir_all(&main_cq).unwrap();
        fs::write(main_cq.join("config.json"), r#"{"timeout": 999}"#).unwrap();

        // Create a worktree
        let wt_dir = tempfile::tempdir().unwrap();
        let wt_path = wt_dir.path().join("wt");
        Command::new("git")
            .args([
                "worktree",
                "add",
                wt_path.to_str().unwrap(),
                "-b",
                "wt-branch",
            ])
            .current_dir(main_dir.path())
            .output()
            .unwrap();

        // Without local config, worktree should resolve to main repo
        let canonical_main = fs::canonicalize(main_dir.path()).unwrap();
        assert_eq!(resolve_project_root(&wt_path), canonical_main);

        // Now add a local .cq/config.json in the worktree — it should win
        let wt_cq = wt_path.join(".cq");
        fs::create_dir_all(&wt_cq).unwrap();
        fs::write(wt_cq.join("config.json"), r#"{"timeout": 111}"#).unwrap();
        assert_eq!(resolve_project_root(&wt_path), wt_path);
    }

    #[test]
    fn test_resolve_project_root_non_git_dir() {
        // A temp dir with no git repo and no .cq/config.json — should return cwd
        let dir = tempfile::tempdir().unwrap();
        let result = resolve_project_root(dir.path());
        assert_eq!(result, dir.path());
    }
}
