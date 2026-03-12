use crate::config;
use crate::db::Db;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Start a brand new sub-agent session.
pub fn start(
    prompt: &str,
    name: Option<&str>,
    cwd: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let args = vec![
        "-p".to_string(),
        "--session-id".to_string(),
        session_id.clone(),
        prompt.to_string(),
    ];
    launch(&session_id, Some(&session_id), name, prompt, cwd, args)
}

/// Resume a session. Accepts either a cq session ID prefix or a raw claude session ID.
/// Looks up the claude_session_id from the DB if it's a cq prefix, otherwise uses it directly.
pub fn resume(
    id_or_prefix: &str,
    prompt: &str,
    cwd: &str,
) -> Result<String, Box<dyn std::error::Error>> {
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
        "--session-id".to_string(),
        claude_sid.clone(),
        prompt.to_string(),
    ];
    // Inherit the name from the original session if it had one
    let name = if let Some(sess) = db.find_session(id_or_prefix)? {
        sess.name
    } else {
        None
    };

    let display_prompt = format!(
        "[resumed {}] {}",
        &claude_sid[..8.min(claude_sid.len())],
        prompt
    );
    launch(
        &cq_session_id,
        Some(&claude_sid),
        name.as_deref(),
        &display_prompt,
        cwd,
        args,
    )
}

/// Common launch logic for both start and resume.
fn launch(
    session_id: &str,
    claude_session_id: Option<&str>,
    name: Option<&str>,
    prompt_display: &str,
    cwd: &str,
    extra_args: Vec<String>,
) -> Result<String, Box<dyn std::error::Error>> {
    let cwd_abs = fs::canonicalize(cwd)?;
    let project_root = resolve_project_root(&cwd_abs);
    let db_path = config::db_path();
    let log_dir = config::log_dir();
    fs::create_dir_all(&log_dir)?;

    let log_path = log_dir.join(format!("{session_id}.log"));
    let log_file = fs::File::create(&log_path)?;
    let stderr_path = log_dir.join(format!("{session_id}.stderr"));
    let stderr_file = fs::File::create(&stderr_path)?;

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

    let mut cmd = Command::new("claude");
    for arg in &extra_args {
        cmd.arg(arg);
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

    let pid = child.id();

    // Record in DB
    let db = Db::open(&db_path)?;
    db.create_session(
        session_id,
        claude_session_id,
        name,
        prompt_display,
        &cwd_abs.to_string_lossy(),
        pid,
    )?;

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
                    let st = if code == Some(0) {
                        "completed"
                    } else {
                        "failed"
                    };
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
        fs::write(
            main_cq.join("config.json"),
            r#"{"timeout": 999}"#,
        )
        .unwrap();

        // Create a worktree
        let wt_dir = tempfile::tempdir().unwrap();
        let wt_path = wt_dir.path().join("wt");
        Command::new("git")
            .args(["worktree", "add", wt_path.to_str().unwrap(), "-b", "wt-branch"])
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
