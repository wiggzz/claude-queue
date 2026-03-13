use crate::backend::AgentBackend;
use crate::config;
use crate::db::Db;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub enum PushResult {
    Started(String),
    Queued,
    Resumed(String),
}

pub fn push(prompt: &str, name: &str, cwd: &str) -> Result<PushResult, Box<dyn std::error::Error>> {
    let db = Db::open(&config::db_path())?;

    if let Some(sess) = db.find_session(name)? {
        if let Ok(cwd_abs) = fs::canonicalize(cwd)
            && cwd_abs.to_string_lossy() != sess._cwd
        {
            eprintln!(
                "warning: --cwd '{}' differs from session's original cwd '{}'. Resume will use the original cwd.",
                cwd_abs.display(),
                sess._cwd,
            );
        }

        let alive = sess.pid.map(is_pid_alive).unwrap_or(false);
        if (sess.status == "running" && alive) || sess.status == "delivering" {
            let cwd_abs = fs::canonicalize(cwd)?;
            db.push_queued_message(name, prompt, Some(&cwd_abs.to_string_lossy()))?;
            return Ok(PushResult::Queued);
        }

        if !db.claim_session_for_delivery(&sess.session_id)? {
            let cwd_abs = fs::canonicalize(cwd)?;
            db.push_queued_message(name, prompt, Some(&cwd_abs.to_string_lossy()))?;
            return Ok(PushResult::Queued);
        }

        let mut messages = db.take_all_queued_messages(name)?;
        messages.push((prompt.to_string(), Some(cwd.to_string())));
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
        let session_id = resume_session(name, &sess, &combined_prompt, resume_cwd)?;
        let _ = db.update_session_status(&sess.session_id, &sess.status, sess._exit_code);
        return Ok(PushResult::Resumed(session_id));
    }

    let session_id = start(prompt, Some(name), cwd, None)?;
    Ok(PushResult::Started(session_id))
}

pub fn interrupt(
    name: &str,
    prompt: &str,
    cwd: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let db = Db::open(&config::db_path())?;
    let sess = db
        .find_session(name)?
        .ok_or_else(|| format!("No session found: {name}"))?;

    if let Some(pid) = sess.pid
        && pid > 0
        && is_pid_alive(pid)
    {
        kill_session(pid)?;
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let _ = db.update_session_status(&sess.session_id, "killed", None);

    let cleared = db.clear_queued_messages(name)?;
    if cleared > 0 {
        eprintln!("Cleared {cleared} queued message(s) for session {name}.");
    }

    resume_session(name, &sess, prompt, cwd)
}

pub fn cancel_queued(name: &str) -> Result<usize, Box<dyn std::error::Error>> {
    let db = Db::open(&config::db_path())?;
    Ok(db.clear_queued_messages(name)?)
}

/// Start a brand new sub-agent session.
pub fn start(
    prompt: &str,
    name: Option<&str>,
    cwd: &str,
    backend: Option<AgentBackend>,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(name) = name {
        let db = Db::open(&config::db_path())?;
        if let Some(sess) = db.find_session(name)? {
            let alive = sess.pid.map(is_pid_alive).unwrap_or(false);
            if sess.status == "running" && alive {
                return Err(format!(
                    "Session '{name}' is already running. Use `cq push {name} \"...\"` to queue a message, or `cq interrupt {name} \"...\"` to kill and restart."
                )
                .into());
            }
        }
    }

    let cwd_abs = fs::canonicalize(cwd)?;
    let project_root = resolve_project_root(&cwd_abs);
    let config = config::Config::load(&project_root);
    let backend = select_backend(backend, &config);
    let session_id = uuid::Uuid::new_v4().to_string();
    let agent_session_id = initial_agent_session_id(backend, &session_id);

    launch(
        &session_id,
        backend,
        &agent_session_id,
        name,
        prompt,
        prompt,
        &cwd_abs,
    )
}

/// Resume a session. Accepts either a cq session ID prefix or a raw backend session ID.
pub fn resume(
    id_or_prefix: &str,
    prompt: &str,
    cwd: &str,
    backend: Option<AgentBackend>,
) -> Result<String, Box<dyn std::error::Error>> {
    let db = Db::open(&config::db_path())?;
    let input_cwd_abs = fs::canonicalize(cwd)?;
    let project_root = resolve_project_root(&input_cwd_abs);
    let config = config::Config::load(&project_root);

    let (backend, agent_session_id, name, cwd_abs) =
        if let Some(sess) = db.find_session(id_or_prefix)? {
            if let Some(requested_backend) = backend
                && requested_backend != sess.agent_backend
            {
                return Err(format!(
                    "Session '{}' uses backend '{}', not '{}'",
                    id_or_prefix,
                    sess.agent_backend.as_str(),
                    requested_backend.as_str()
                )
                .into());
            }

            let agent_session_id = sess
                .agent_session_id
                .clone()
                .or(sess.claude_session_id.clone())
                .unwrap_or_else(|| {
                    if sess.agent_backend == AgentBackend::Claude {
                        sess.session_id.clone()
                    } else {
                        id_or_prefix.to_string()
                    }
                });

            (
                sess.agent_backend,
                agent_session_id,
                sess.name,
                PathBuf::from(&sess._cwd),
            )
        } else {
            (
                select_backend(backend, &config),
                id_or_prefix.to_string(),
                None,
                input_cwd_abs,
            )
        };

    let cq_session_id = uuid::Uuid::new_v4().to_string();
    let display_prompt = format!(
        "[resumed {}] {}",
        &agent_session_id[..8.min(agent_session_id.len())],
        prompt
    );
    launch(
        &cq_session_id,
        backend,
        &agent_session_id,
        name.as_deref(),
        prompt,
        &display_prompt,
        &cwd_abs,
    )
}

fn resume_session(
    name: &str,
    prev_session: &crate::db::Session,
    prompt: &str,
    _cwd: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let cwd_abs = PathBuf::from(&prev_session._cwd);
    let backend = prev_session.agent_backend;
    let agent_session_id = prev_session
        .agent_session_id
        .clone()
        .or(prev_session.claude_session_id.clone())
        .unwrap_or_else(|| prev_session.session_id.clone());

    let cq_session_id = uuid::Uuid::new_v4().to_string();
    let display_prompt = format!(
        "[resumed {}] {}",
        &agent_session_id[..8.min(agent_session_id.len())],
        prompt
    );
    launch(
        &cq_session_id,
        backend,
        &agent_session_id,
        Some(name),
        prompt,
        &display_prompt,
        &cwd_abs,
    )
}

fn select_backend(explicit: Option<AgentBackend>, config: &config::Config) -> AgentBackend {
    if let Some(backend) = explicit {
        return backend;
    }
    if let Ok(value) = std::env::var("CQ_AGENT_BACKEND")
        && let Some(backend) = AgentBackend::parse_env(&value)
    {
        return backend;
    }
    config.default_backend
}

fn initial_agent_session_id(backend: AgentBackend, session_id: &str) -> String {
    match backend {
        AgentBackend::Claude => session_id.to_string(),
        AgentBackend::Pi => pi_session_file(session_id).to_string_lossy().into_owned(),
    }
}

fn launch(
    session_id: &str,
    backend: AgentBackend,
    agent_session_id: &str,
    name: Option<&str>,
    prompt: &str,
    prompt_display: &str,
    cwd_abs: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let db_path = config::db_path();
    let log_dir = config::log_dir();
    fs::create_dir_all(&log_dir)?;

    let db = Db::open(&db_path)?;
    db.create_session(
        session_id,
        backend,
        Some(agent_session_id),
        name,
        prompt_display,
        &cwd_abs.to_string_lossy(),
        None,
    )?;

    let cq_bin = std::env::current_exe().unwrap_or_else(|_| "cq".into());
    let wrapper_stderr_path = log_dir.join(format!("{session_id}.wrapper.stderr"));
    let wrapper_stderr = fs::File::create(&wrapper_stderr_path)?;

    let mut cmd = Command::new(&cq_bin);
    cmd.args([
        "run-session",
        session_id,
        "--backend",
        backend.as_str(),
        "--agent-session-id",
        agent_session_id,
        "--cwd",
        &cwd_abs.to_string_lossy(),
        "--prompt-display",
        prompt_display,
        "--prompt",
        prompt,
    ]);
    if let Some(name) = name {
        cmd.args(["--name", name]);
    }

    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(wrapper_stderr)
        .spawn()?;

    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let check_db = Db::open(&db_path)?;
        if let Some(sess) = check_db.find_session(session_id)?
            && sess.pid.is_some()
        {
            return Ok(session_id.to_string());
        }
    }

    Ok(session_id.to_string())
}

pub fn run_session(
    session_id: &str,
    backend: AgentBackend,
    agent_session_id: &str,
    name: Option<&str>,
    cwd: &str,
    prompt_display: &str,
    prompt: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let cwd_abs = fs::canonicalize(cwd)?;
    let project_root = resolve_project_root(&cwd_abs);
    let db_path = config::db_path();
    let log_dir = config::log_dir();
    fs::create_dir_all(&log_dir)?;

    let log_path = log_dir.join(format!("{session_id}.log"));
    let stderr_path = log_dir.join(format!("{session_id}.stderr"));
    let stderr_file = fs::File::create(&stderr_path)?;

    let cq_bin = std::env::current_exe().unwrap_or_else(|_| "cq".into());
    let run_config = config::Config::load(&project_root);
    let mut invocation =
        build_backend_command(&cq_bin, backend, session_id, agent_session_id, prompt)?;
    let session_model = run_config.model_for_backend(backend).to_string();
    if !session_model.is_empty() {
        invocation.args.extend(["--model".into(), session_model]);
    }

    let mut child = if should_use_script_pty(backend) {
        build_script_command(&log_path, &invocation)?
    } else {
        fs::File::create(&log_path)?;
        let log_file = fs::File::create(&log_path)?;
        let mut cmd = Command::new(&invocation.program);
        cmd.args(&invocation.args);
        for key in &invocation.env_remove {
            cmd.env_remove(key);
        }
        cmd.stdout(log_file);
        cmd
    }
    .env("CQ_MANAGED", "1")
    .env("CQ_AGENT_BACKEND", backend.as_str())
    .env("CQ_SESSION_ID", session_id)
    .env("CQ_BIN", cq_bin.to_string_lossy().as_ref())
    .env("CQ_DB", db_path.to_string_lossy().as_ref())
    .env("CQ_PROJECT_DIR", project_root.to_string_lossy().as_ref())
    .env("CQ_SESSION_CWD", cwd_abs.to_string_lossy().as_ref())
    .env("CQ_SESSION_NAME", name.unwrap_or(""))
    .env("CQ_SESSION_PROMPT", prompt_display)
    .current_dir(&cwd_abs)
    .stderr(stderr_file)
    .spawn()?;

    let db = Db::open(&db_path)?;
    db.update_session_pid(session_id, child.id())?;

    let (final_status, exit_code) = match child.wait() {
        Ok(status) => {
            let code = status.code();
            let session_status = if code == Some(0) {
                "completed"
            } else {
                "failed"
            };
            (session_status, code)
        }
        Err(_) => ("failed", None),
    };

    let _ = db.update_session_status(session_id, final_status, exit_code);

    if let Some(name) = name {
        if !db.claim_session_for_delivery(session_id)? {
            return Ok(());
        }

        let messages = db.take_all_queued_messages(name)?;
        if messages.is_empty() {
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
            match resume_session(
                name,
                &db.find_session(name)?.ok_or("Session disappeared")?,
                &combined_prompt,
                resume_cwd,
            ) {
                Ok(_) | Err(_) => {
                    let _ = db.update_session_status(session_id, final_status, exit_code);
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
    let db = Db::open(&config::db_path())?;
    let sess = db
        .find_session(session_id)?
        .ok_or_else(|| format!("No session matching '{session_id}'"))?;
    let log_path = config::log_dir().join(format!("{}.log", sess.session_id));
    if !log_path.exists() {
        return Err(format!("No log file found for session {}", sess.session_id).into());
    }
    let raw = fs::read_to_string(&log_path)?;
    Ok(sess.agent_backend.extract_output(&raw))
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

struct BackendInvocation {
    program: String,
    args: Vec<String>,
    env_remove: Vec<&'static str>,
}

fn build_backend_command(
    cq_bin: &Path,
    backend: AgentBackend,
    session_id: &str,
    agent_session_id: &str,
    prompt: &str,
) -> Result<BackendInvocation, Box<dyn std::error::Error>> {
    match backend {
        AgentBackend::Claude => build_claude_command(cq_bin, session_id, agent_session_id, prompt),
        AgentBackend::Pi => build_pi_command(cq_bin, session_id, agent_session_id, prompt),
    }
}

fn build_claude_command(
    cq_bin: &Path,
    session_id: &str,
    agent_session_id: &str,
    prompt: &str,
) -> Result<BackendInvocation, Box<dyn std::error::Error>> {
    let hook_command = format!("{} hook claude", cq_bin.display());
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

    let claude_bin = std::env::var("CQ_CLAUDE_BIN").unwrap_or_else(|_| "claude".into());
    let mut args = vec!["-p".into()];
    if session_id == agent_session_id {
        args.extend(["--session-id".into(), agent_session_id.into()]);
    } else {
        args.extend(["--resume".into(), agent_session_id.into()]);
    }
    args.extend([
        prompt.into(),
        "--settings".into(),
        settings_str,
        "--permission-mode".into(),
        "bypassPermissions".into(),
        "--dangerously-skip-permissions".into(),
    ]);
    Ok(BackendInvocation {
        program: claude_bin,
        args,
        env_remove: vec!["CLAUDECODE"],
    })
}

fn build_pi_command(
    cq_bin: &Path,
    session_id: &str,
    agent_session_id: &str,
    prompt: &str,
) -> Result<BackendInvocation, Box<dyn std::error::Error>> {
    let hook_dir = config::log_dir().join("pi-hooks").join(session_id);
    fs::create_dir_all(&hook_dir)?;
    let extension_path = hook_dir.join("cq-pi-hook.ts");
    write_pi_extension(cq_bin, &extension_path)?;

    let session_file = PathBuf::from(agent_session_id);
    if let Some(parent) = session_file.parent() {
        fs::create_dir_all(parent)?;
    }

    let pi_bin = std::env::var("CQ_PI_BIN").unwrap_or_else(|_| "pi".into());
    Ok(BackendInvocation {
        program: pi_bin,
        args: vec![
            "--print".into(),
            "--session".into(),
            session_file.to_string_lossy().into_owned(),
            "--no-extensions".into(),
            "--extension".into(),
            extension_path.to_string_lossy().into_owned(),
            prompt.into(),
        ],
        env_remove: Vec::new(),
    })
}

fn should_use_script_pty(backend: AgentBackend) -> bool {
    cfg!(unix)
        && matches!(backend, AgentBackend::Claude)
        && std::env::var("CQ_DISABLE_PTY").is_err()
}

fn build_script_command(
    log_path: &Path,
    invocation: &BackendInvocation,
) -> Result<Command, Box<dyn std::error::Error>> {
    let mut cmd = Command::new("script");
    cmd.arg("-q").arg(log_path);
    cmd.arg(&invocation.program);
    cmd.args(&invocation.args);
    for key in &invocation.env_remove {
        cmd.env_remove(key);
    }
    cmd.stdout(Stdio::null());
    Ok(cmd)
}

fn pi_session_file(session_id: &str) -> PathBuf {
    config::log_dir()
        .join("pi-sessions")
        .join(format!("{session_id}.jsonl"))
}

fn write_pi_extension(
    cq_bin: &Path,
    extension_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let cq_bin = js_string(&cq_bin.to_string_lossy());
    let script = format!(
        r#"import {{ spawnSync }} from "node:child_process";

export default function (pi) {{
  pi.on("tool_call", async (event) => {{
    const cqBin = process.env.CQ_BIN || {cq_bin};
    const result = spawnSync(cqBin, ["hook", "pi"], {{
      input: JSON.stringify({{ toolName: event.toolName, input: event.input }}),
      env: process.env,
      encoding: "utf8",
    }});

    if (result.error) {{
      return {{ block: true, reason: result.error.message }};
    }}

    if (result.status !== 0) {{
      const stderr = (result.stderr || "").trim();
      return {{ block: true, reason: stderr || `cq hook pi failed with exit code ${{result.status}}` }};
    }}

    let decision = null;
    try {{
      decision = JSON.parse(result.stdout || "{{}}");
    }} catch (error) {{
      return {{ block: true, reason: `Invalid cq hook response: ${{error.message}}` }};
    }}

    if (decision.decision === "deny") {{
      return {{ block: true, reason: decision.reason || "Denied by cq" }};
    }}
  }});
}}
"#
    );
    fs::write(extension_path, script)?;
    Ok(())
}

fn js_string(input: &str) -> String {
    serde_json::to_string(input).unwrap_or_else(|_| "\"cq\"".into())
}

/// Resolve the project root directory, handling git worktrees.
/// If `cwd` itself contains `.cq/config.json`, it is used directly (local config wins).
/// Otherwise, for worktrees, returns the main repository's root.
/// Falls back to `git rev-parse --show-toplevel`, then to the given cwd.
pub fn resolve_project_root(cwd: &Path) -> PathBuf {
    if cwd.join(".cq").join("config.json").exists() {
        return cwd.to_path_buf();
    }

    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(cwd)
        .output()
        && output.status.success()
    {
        let common_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let common_path = Path::new(&common_dir);
        if let Some(root) = common_path.parent() {
            return root.to_path_buf();
        }
    }

    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        && output.status.success()
    {
        let toplevel = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !toplevel.is_empty() {
            return PathBuf::from(toplevel);
        }
    }

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
    fn test_select_backend_prefers_env() {
        let _guard = EnvGuard::set("CQ_AGENT_BACKEND", Some("pi"));
        let config = config::Config::default();
        assert_eq!(select_backend(None, &config), AgentBackend::Pi);
    }

    #[test]
    fn test_resolve_project_root_prefers_local_config() {
        let dir = tempfile::tempdir().unwrap();
        let cq_dir = dir.path().join(".cq");
        fs::create_dir_all(&cq_dir).unwrap();
        fs::write(cq_dir.join("config.json"), r#"{"timeout": 100}"#).unwrap();

        let result = resolve_project_root(dir.path());
        assert_eq!(result, dir.path());
    }

    #[test]
    fn test_resolve_project_root_falls_back_to_git_root() {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let sub = dir.path().join("subdir");
        fs::create_dir_all(&sub).unwrap();

        let result = resolve_project_root(&sub);
        let canonical_dir = fs::canonicalize(dir.path()).unwrap();
        assert_eq!(result, canonical_dir);
    }

    #[test]
    fn test_resolve_project_root_local_config_beats_worktree_parent() {
        let main_dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(main_dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=Test User",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .current_dir(main_dir.path())
            .output()
            .unwrap();
        let main_cq = main_dir.path().join(".cq");
        fs::create_dir_all(&main_cq).unwrap();
        fs::write(main_cq.join("config.json"), r#"{"timeout": 999}"#).unwrap();

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

        let canonical_main = fs::canonicalize(main_dir.path()).unwrap();
        assert_eq!(resolve_project_root(&wt_path), canonical_main);

        let wt_cq = wt_path.join(".cq");
        fs::create_dir_all(&wt_cq).unwrap();
        fs::write(wt_cq.join("config.json"), r#"{"timeout": 111}"#).unwrap();
        assert_eq!(resolve_project_root(&wt_path), wt_path);
    }

    #[test]
    fn test_resolve_project_root_non_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = resolve_project_root(dir.path());
        assert_eq!(result, dir.path());
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
            EnvGuard { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }
}
