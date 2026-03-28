use crate::backend::AgentBackend;
use crate::config;
use crate::db::Db;
use rusqlite::Error as SqliteError;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub enum PushResult {
    Started(String),
    Queued,
    Resumed(String),
}

fn project_root_for_cwd(cwd: &Path) -> PathBuf {
    config::resolve_project_dir(cwd)
}

fn db_path_for_cwd(cwd: &Path) -> PathBuf {
    config::db_path_for(&project_root_for_cwd(cwd))
}

pub fn push(prompt: &str, name: &str, cwd: &str) -> Result<PushResult, Box<dyn std::error::Error>> {
    let cwd_abs = fs::canonicalize(cwd)?;
    let db = Db::open(&db_path_for_cwd(&cwd_abs))?;

    if let Some(sess) = db.find_session(name)? {
        if cwd_abs.to_string_lossy() != sess._cwd {
            eprintln!(
                "warning: --cwd '{}' differs from session's original cwd '{}'. Resume will use the original cwd.",
                cwd_abs.display(),
                sess._cwd,
            );
        }

        let alive = sess.pid.map(is_pid_alive).unwrap_or(false);
        if (sess.status == "running" && alive) || sess.status == "delivering" {
            db.push_queued_message(name, prompt, Some(&cwd_abs.to_string_lossy()))?;
            return Ok(PushResult::Queued);
        }

        if !db.claim_session_for_delivery(&sess.session_id)? {
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
    let cwd_abs = fs::canonicalize(cwd)?;
    let db = Db::open(&db_path_for_cwd(&cwd_abs))?;
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
    let cwd = std::env::current_dir()?;
    let db = Db::open(&db_path_for_cwd(&cwd))?;
    Ok(db.clear_queued_messages(name)?)
}

/// Start a brand new sub-agent session.
pub fn start(
    prompt: &str,
    name: Option<&str>,
    cwd: &str,
    backend: Option<AgentBackend>,
) -> Result<String, Box<dyn std::error::Error>> {
    let cwd_abs = fs::canonicalize(cwd)?;
    let project_root = project_root_for_cwd(&cwd_abs);

    if let Some(name) = name {
        let db = Db::open(&config::db_path_for(&project_root))?;
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
    let input_cwd_abs = fs::canonicalize(cwd)?;
    let project_root = project_root_for_cwd(&input_cwd_abs);
    let db = Db::open(&config::db_path_for(&project_root))?;
    let config = config::Config::load(&project_root);
    let sessions = db.get_sessions()?;

    let (backend, agent_session_id, name, cwd_abs) = resolve_resume_target(
        &sessions,
        id_or_prefix,
        backend,
        select_backend(backend, &config),
        &input_cwd_abs,
    )
    .map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;

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

fn resolve_resume_target(
    sessions: &[crate::db::Session],
    query: &str,
    requested_backend: Option<AgentBackend>,
    default_backend: AgentBackend,
    input_cwd_abs: &Path,
) -> Result<(AgentBackend, String, Option<String>, PathBuf), String> {
    if let Some(session) = resolve_managed_resume_session(sessions, query)? {
        if let Some(requested_backend) = requested_backend
            && requested_backend != session.agent_backend
        {
            return Err(format!(
                "Session '{}' uses backend '{}', not '{}'",
                query,
                session.agent_backend.as_str(),
                requested_backend.as_str()
            ));
        }

        let agent_session_id = session
            .agent_session_id
            .clone()
            .or(session.claude_session_id.clone())
            .unwrap_or_else(|| {
                if session.agent_backend == AgentBackend::Claude {
                    session.session_id.clone()
                } else {
                    query.to_string()
                }
            });

        return Ok((
            session.agent_backend,
            agent_session_id,
            session.name.clone(),
            PathBuf::from(&session._cwd),
        ));
    }

    if default_backend == AgentBackend::Claude && looks_like_claude_session_id(query) {
        return Ok((
            default_backend,
            query.to_string(),
            None,
            input_cwd_abs.to_path_buf(),
        ));
    }

    Err(format!(
        "No session found for '{query}'. Use a cq session name, cq session ID prefix, or a full Claude session UUID."
    ))
}

fn resolve_managed_resume_session<'a>(
    sessions: &'a [crate::db::Session],
    query: &str,
) -> Result<Option<&'a crate::db::Session>, String> {
    if let Some(session) = sessions
        .iter()
        .find(|session| session.name.as_deref() == Some(query))
    {
        return Ok(Some(session));
    }

    if let Some(session) = sessions.iter().find(|session| session.session_id == query) {
        return Ok(Some(session));
    }

    let session_id_prefix_matches: Vec<_> = sessions
        .iter()
        .filter(|session| session.session_id.starts_with(query))
        .collect();
    match session_id_prefix_matches.len() {
        0 => {}
        1 => return Ok(Some(session_id_prefix_matches[0])),
        _ => {
            let ids = session_id_prefix_matches
                .iter()
                .map(|session| session.session_id[..8.min(session.session_id.len())].to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "Session prefix '{query}' is ambiguous; matches cq sessions: {ids}"
            ));
        }
    }

    Ok(sessions.iter().find(|session| {
        session.agent_session_id.as_deref() == Some(query)
            || session.claude_session_id.as_deref() == Some(query)
    }))
}

fn looks_like_claude_session_id(query: &str) -> bool {
    uuid::Uuid::parse_str(query).is_ok()
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

const SESSION_DB_WRITE_RETRIES: usize = 6;
const SESSION_DB_WRITE_RETRY_DELAY: Duration = Duration::from_secs(1);

fn launch(
    session_id: &str,
    backend: AgentBackend,
    agent_session_id: &str,
    name: Option<&str>,
    prompt: &str,
    prompt_display: &str,
    cwd_abs: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let project_root = project_root_for_cwd(cwd_abs);
    let db_path = config::db_path_for(&project_root);
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
    let project_root = project_root_for_cwd(&cwd_abs);
    let db_path = config::db_path_for(&project_root);
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
            let session_status = classify_terminal_status(code, &log_path, &stderr_path);
            (session_status, code)
        }
        Err(_) => ("failed", None),
    };

    update_session_status_with_retry(&db_path, session_id, final_status, exit_code)?;

    if let Some(name) = name {
        if !db.claim_session_for_delivery(session_id)? {
            return Ok(());
        }

        let messages = db.take_all_queued_messages(name)?;
        if messages.is_empty() {
            update_session_status_with_retry(&db_path, session_id, final_status, exit_code)?;
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
                    update_session_status_with_retry(
                        &db_path,
                        session_id,
                        final_status,
                        exit_code,
                    )?;
                }
            }
        }
    }

    Ok(())
}

fn update_session_status_with_retry(
    db_path: &Path,
    session_id: &str,
    status: &str,
    exit_code: Option<i32>,
) -> Result<(), Box<dyn std::error::Error>> {
    update_session_status_with_retry_policy(
        db_path,
        session_id,
        status,
        exit_code,
        SESSION_DB_WRITE_RETRIES,
        SESSION_DB_WRITE_RETRY_DELAY,
    )
}

fn update_session_status_with_retry_policy(
    db_path: &Path,
    session_id: &str,
    status: &str,
    exit_code: Option<i32>,
    max_attempts: usize,
    retry_delay: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut last_err: Option<rusqlite::Error> = None;

    for attempt in 1..=max_attempts.max(1) {
        match Db::open(db_path)?.update_session_status(session_id, status, exit_code) {
            Ok(()) => return Ok(()),
            Err(err) if is_retryable_sqlite_error(&err) && attempt < max_attempts.max(1) => {
                last_err = Some(err);
                std::thread::sleep(retry_delay);
            }
            Err(err) => return Err(err.into()),
        }
    }

    Err(last_err.unwrap_or_else(|| SqliteError::InvalidQuery).into())
}

fn is_retryable_sqlite_error(err: &rusqlite::Error) -> bool {
    match err {
        SqliteError::SqliteFailure(code, _) => {
            matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
        }
        _ => false,
    }
}

/// Check the log file for a dead session and update DB status accordingly.
pub fn resolve_dead_session(db: &crate::db::Db, session_id: &str) -> String {
    let log_path = config::log_dir().join(format!("{session_id}.log"));
    let stderr_path = config::log_dir().join(format!("{session_id}.stderr"));
    let status = classify_terminal_status(None, &log_path, &stderr_path);
    let _ = db.update_session_status(session_id, status, None);
    status.to_string()
}

fn classify_terminal_status(
    exit_code: Option<i32>,
    log_path: &Path,
    stderr_path: &Path,
) -> &'static str {
    if exit_code == Some(0) {
        return "completed";
    }

    let log = fs::read_to_string(log_path).ok();
    let stderr = fs::read_to_string(stderr_path).unwrap_or_default();
    infer_status_from_artifacts(log.as_deref(), &stderr)
}

fn infer_status_from_artifacts(log: Option<&str>, stderr: &str) -> &'static str {
    let has_log_output = log.is_some_and(|content| !content.trim().is_empty());
    let stderr_trimmed = stderr.trim();

    if has_log_output {
        if stderr_looks_fatal(stderr_trimmed) {
            "failed"
        } else {
            "completed"
        }
    } else if stderr_trimmed.is_empty() {
        "completed"
    } else {
        "failed"
    }
}

fn stderr_looks_fatal(stderr: &str) -> bool {
    let stderr_lower = stderr.to_ascii_lowercase();
    stderr_lower.contains("error:")
        || stderr_lower.contains("fatal:")
        || stderr_lower.contains("panic")
        || stderr_lower.contains("exception")
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

        let result = config::resolve_project_dir(dir.path());
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

        let result = config::resolve_project_dir(&sub);
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
        assert_eq!(config::resolve_project_dir(&wt_path), canonical_main);

        let wt_cq = wt_path.join(".cq");
        fs::create_dir_all(&wt_cq).unwrap();
        fs::write(wt_cq.join("config.json"), r#"{"timeout": 111}"#).unwrap();
        assert_eq!(config::resolve_project_dir(&wt_path), wt_path);
    }

    #[test]
    fn test_resolve_project_root_non_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = config::resolve_project_dir(dir.path());
        assert_eq!(result, dir.path());
    }

    fn test_session(
        session_id: &str,
        agent_backend: AgentBackend,
        agent_session_id: Option<&str>,
        name: Option<&str>,
        cwd: &str,
    ) -> crate::db::Session {
        crate::db::Session {
            _id: 0,
            session_id: session_id.to_string(),
            agent_backend,
            agent_session_id: agent_session_id.map(str::to_string),
            claude_session_id: (agent_backend == AgentBackend::Claude)
                .then(|| agent_session_id.map(str::to_string))
                .flatten(),
            name: name.map(str::to_string),
            prompt: "prompt".to_string(),
            _cwd: cwd.to_string(),
            status: "completed".to_string(),
            pid: None,
            started_at: "2026-03-25T00:00:00Z".to_string(),
            _completed_at: None,
            _exit_code: Some(0),
        }
    }

    #[test]
    fn test_resolve_resume_target_prefers_exact_managed_backend_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = vec![
            test_session(
                "cq-newer",
                AgentBackend::Claude,
                Some("11111111-1111-1111-1111-111111111111"),
                Some("task"),
                "/managed",
            ),
            test_session(
                "11111111-prefix-collision",
                AgentBackend::Claude,
                Some("other-native-session"),
                Some("other"),
                "/other",
            ),
        ];

        let (_, agent_session_id, name, cwd) = resolve_resume_target(
            &sessions,
            "11111111-1111-1111-1111-111111111111",
            None,
            AgentBackend::Claude,
            dir.path(),
        )
        .unwrap();

        assert_eq!(agent_session_id, "11111111-1111-1111-1111-111111111111");
        assert_eq!(name.as_deref(), Some("task"));
        assert_eq!(cwd, PathBuf::from("/managed"));
    }

    #[test]
    fn test_resolve_resume_target_rejects_ambiguous_cq_session_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = vec![
            test_session(
                "abc12345-old",
                AgentBackend::Claude,
                Some("native-1"),
                None,
                "/one",
            ),
            test_session(
                "abc12345-new",
                AgentBackend::Claude,
                Some("native-2"),
                None,
                "/two",
            ),
        ];

        let err = resolve_resume_target(
            &sessions,
            "abc12345",
            None,
            AgentBackend::Claude,
            dir.path(),
        )
        .unwrap_err();

        assert!(err.contains("ambiguous"), "unexpected error: {err}");
    }

    #[test]
    fn test_resolve_resume_target_allows_unmanaged_full_claude_session_uuid() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = vec![test_session(
            "cq-session",
            AgentBackend::Claude,
            Some("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"),
            Some("task"),
            "/managed",
        )];
        let native_id = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";

        let (backend, agent_session_id, name, cwd) =
            resolve_resume_target(&sessions, native_id, None, AgentBackend::Claude, dir.path())
                .unwrap();

        assert_eq!(backend, AgentBackend::Claude);
        assert_eq!(agent_session_id, native_id);
        assert!(name.is_none());
        assert_eq!(cwd, dir.path());
    }

    #[test]
    fn test_resolve_resume_target_rejects_non_uuid_unmanaged_query() {
        let dir = tempfile::tempdir().unwrap();
        let err =
            resolve_resume_target(&[], "not-a-session", None, AgentBackend::Claude, dir.path())
                .unwrap_err();
        assert!(err.contains("No session found"), "unexpected error: {err}");
    }

    #[test]
    fn test_update_session_status_retries_through_temporary_db_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("cq.db");
        let db = Db::open(&db_path).unwrap();
        db.create_session(
            "locked-session",
            AgentBackend::Claude,
            Some("locked-session"),
            Some("lock-test"),
            "prompt",
            tmp.path().to_str().unwrap(),
            Some(1234),
        )
        .unwrap();
        drop(db);

        let lock_path = db_path.clone();
        let lock_thread = std::thread::spawn(move || {
            let conn = rusqlite::Connection::open(lock_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode=WAL; BEGIN EXCLUSIVE;")
                .unwrap();
            std::thread::sleep(Duration::from_secs(6));
            conn.execute_batch("COMMIT;").unwrap();
        });

        std::thread::sleep(Duration::from_millis(200));

        update_session_status_with_retry_policy(
            &db_path,
            "locked-session",
            "completed",
            Some(0),
            2,
            Duration::from_millis(200),
        )
        .unwrap();

        lock_thread.join().unwrap();

        let db = Db::open(&db_path).unwrap();
        let session = db.find_session("locked-session").unwrap().unwrap();
        assert_eq!(session.status, "completed");
        assert_eq!(session._exit_code, Some(0));
    }

    #[test]
    fn test_infer_status_from_artifacts_treats_output_only_nonzero_exit_as_completed() {
        assert_eq!(
            infer_status_from_artifacts(Some("finished command"), ""),
            "completed"
        );
    }

    #[test]
    fn test_infer_status_from_artifacts_treats_error_stderr_as_failed() {
        assert_eq!(
            infer_status_from_artifacts(Some("finished command"), "Fatal: backend crashed"),
            "failed"
        );
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
