use rusqlite::Connection;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

fn cq_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cq")
}

struct TestEnv {
    home_dir: tempfile::TempDir,
    project_dir: tempfile::TempDir,
    bin_dir: tempfile::TempDir,
    db_path: PathBuf,
}

impl TestEnv {
    fn new(default_backend: Option<&str>) -> Self {
        let home_dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let db_path = home_dir.path().join(".cq").join("cq.db");

        fs::create_dir_all(home_dir.path().join(".cq")).unwrap();
        fs::write(
            home_dir.path().join(".cq").join("config.json"),
            r#"{"timeout":5,"poll_interval":0.05,"policies":[],"supervisor":{"enabled":false}}"#,
        )
        .unwrap();

        let project_config = match default_backend {
            Some(backend) => format!(
                r#"{{"default_backend":"{backend}","timeout":5,"poll_interval":0.05,"policies":[],"supervisor":{{"enabled":false}}}}"#
            ),
            None => {
                r#"{"timeout":5,"poll_interval":0.05,"policies":[],"supervisor":{"enabled":false}}"#
                    .to_string()
            }
        };
        fs::create_dir_all(project_dir.path().join(".cq")).unwrap();
        fs::write(
            project_dir.path().join(".cq").join("config.json"),
            project_config,
        )
        .unwrap();

        TestEnv {
            home_dir,
            project_dir,
            bin_dir,
            db_path,
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(cq_bin());
        cmd.env("HOME", self.home_dir.path())
            .env("CQ_DB", &self.db_path)
            .env("CQ_DISABLE_PTY", "1")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin_dir.path().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .current_dir(self.project_dir.path());
        cmd
    }

    fn install_script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.bin_dir.path().join(name);
        fs::write(&path, body).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }
}

#[test]
fn pi_backend_uses_config_and_resume_reuses_session_file() {
    let env = TestEnv::new(Some("pi"));
    let fake_pi = env.install_script(
        "pi",
        r#"#!/bin/sh
session_file=""
prompt=""
while [ $# -gt 0 ]; do
  case "$1" in
    --session)
      session_file="$2"
      shift 2
      ;;
    --extension)
      shift 2
      ;;
    --print|--no-extensions)
      shift
      ;;
    *)
      prompt="$1"
      shift
      ;;
  esac
done

mkdir -p "$(dirname "$session_file")"
printf '%s\n' "$prompt" >> "$session_file"

if [ "$prompt" = "needs approval" ]; then
  decision=$(printf '{"toolName":"bash","input":{"command":"echo from-pi"}}' | "$CQ_BIN" hook pi)
  echo "$decision" | grep -q '"decision":"deny"' && exit 1
fi

printf 'pi:%s\n' "$prompt"
"#,
    );

    let output = env
        .command()
        .env("CQ_PI_BIN", &fake_pi)
        .args(["start", "needs approval", "--name", "pi-task"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let pending = wait_for_pending(&env, "pi-task", Some(&fake_pi), None);
    assert!(pending.contains("Bash"), "{pending}");

    let approve = env
        .command()
        .env("CQ_PI_BIN", &fake_pi)
        .args(["approve", "all", "--session", "pi-task"])
        .output()
        .unwrap();
    assert!(approve.status.success(), "{approve:?}");

    let wait = env
        .command()
        .env("CQ_PI_BIN", &fake_pi)
        .args(["wait", "pi-task"])
        .output()
        .unwrap();
    assert!(wait.status.success(), "{wait:?}");
    assert!(String::from_utf8_lossy(&wait.stdout).contains("pi:needs approval"));

    let conn = Connection::open(&env.db_path).unwrap();
    let first_session_path: String = conn
        .query_row(
            "SELECT agent_session_id FROM sessions WHERE name = 'pi-task' ORDER BY id ASC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(first_session_path.ends_with(".jsonl"));

    let resume = env
        .command()
        .env("CQ_PI_BIN", &fake_pi)
        .args(["resume", "pi-task", "second prompt"])
        .output()
        .unwrap();
    assert!(resume.status.success(), "{resume:?}");

    let wait = env
        .command()
        .env("CQ_PI_BIN", &fake_pi)
        .args(["wait", "pi-task"])
        .output()
        .unwrap();
    assert!(wait.status.success(), "{wait:?}");
    assert!(String::from_utf8_lossy(&wait.stdout).contains("pi:second prompt"));

    drop(conn);
    let conn = Connection::open(&env.db_path).unwrap();
    let second_session_path: String = conn
        .query_row(
            "SELECT agent_session_id FROM sessions WHERE name = 'pi-task' ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(second_session_path, first_session_path);

    let session_contents = fs::read_to_string(first_session_path).unwrap();
    assert!(session_contents.contains("needs approval"));
    assert!(session_contents.contains("second prompt"));
}

#[test]
fn tail_shows_final_stdout_output_for_completed_pi_session() {
    let env = TestEnv::new(Some("pi"));
    let fake_pi = env.install_script(
        "pi",
        r##"#!/bin/sh
session_file=""
prompt=""
while [ $# -gt 0 ]; do
  case "$1" in
    --session)
      session_file="$2"
      shift 2
      ;;
    --extension)
      shift 2
      ;;
    --print|--no-extensions)
      shift
      ;;
    *)
      prompt="$1"
      shift
      ;;
  esac
done

mkdir -p "$(dirname "$session_file")"
printf '%s\n' '{"type":"message","timestamp":"2026-03-12T14:20:38.000Z","message":{"role":"user","content":[{"type":"text","text":"'"$prompt"'"}]}}' >> "$session_file"

printf 'final stdout: %s\n' "$prompt"
"##,
    );

    let output = env
        .command()
        .env("CQ_PI_BIN", &fake_pi)
        .args(["start", "tail final", "--name", "tail-final-task"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let wait = env
        .command()
        .env("CQ_PI_BIN", &fake_pi)
        .args(["wait", "tail-final-task"])
        .output()
        .unwrap();
    assert!(wait.status.success(), "{wait:?}");

    let tail = env
        .command()
        .env("CQ_PI_BIN", &fake_pi)
        .args(["tail", "tail-final-task"])
        .output()
        .unwrap();
    assert!(tail.status.success(), "{tail:?}");

    let stdout = String::from_utf8_lossy(&tail.stdout);
    assert!(stdout.contains("final stdout: tail final"), "{stdout}");
}

#[test]
fn tail_shows_user_and_assistant_messages_for_completed_pi_session() {
    let env = TestEnv::new(Some("pi"));
    let fake_pi = env.install_script(
        "pi",
        r##"#!/bin/sh
session_file=""
prompt=""
while [ $# -gt 0 ]; do
  case "$1" in
    --session)
      session_file="$2"
      shift 2
      ;;
    --extension)
      shift 2
      ;;
    --print|--no-extensions)
      shift
      ;;
    *)
      prompt="$1"
      shift
      ;;
  esac
done

mkdir -p "$(dirname "$session_file")"
printf '%s\n' '{"type":"message","timestamp":"2026-03-12T14:20:38.000Z","message":{"role":"user","content":[{"type":"text","text":"'"$prompt"'"}]}}' >> "$session_file"
printf '%s\n' '{"type":"message","timestamp":"2026-03-12T14:20:39.000Z","message":{"role":"assistant","content":[{"type":"text","text":"done: '"$prompt"'"}]}}' >> "$session_file"

printf 'pi:%s\n' "$prompt"
"##,
    );

    let output = env
        .command()
        .env("CQ_PI_BIN", &fake_pi)
        .args(["start", "tail me", "--name", "tail-task"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let wait = env
        .command()
        .env("CQ_PI_BIN", &fake_pi)
        .args(["wait", "tail-task"])
        .output()
        .unwrap();
    assert!(wait.status.success(), "{wait:?}");

    let tail = env
        .command()
        .env("CQ_PI_BIN", &fake_pi)
        .args(["tail", "tail-task"])
        .output()
        .unwrap();
    assert!(tail.status.success(), "{tail:?}");

    let stdout = String::from_utf8_lossy(&tail.stdout);
    assert!(stdout.contains("user:"), "{stdout}");
    assert!(stdout.contains("tail me"), "{stdout}");
    assert!(stdout.contains("text:"), "{stdout}");
    assert!(stdout.contains("done: tail me"), "{stdout}");
}

#[test]
fn claude_backend_arg_routes_through_claude_hook() {
    let env = TestEnv::new(None);
    let fake_claude = env.install_script(
        "claude",
        r#"#!/bin/sh
session_id=""
prompt=""
while [ $# -gt 0 ]; do
  case "$1" in
    -p)
      shift
      ;;
    --session-id)
      session_id="$2"
      shift 2
      ;;
    --settings|--permission-mode)
      shift 2
      ;;
    --dangerously-skip-permissions)
      shift
      ;;
    *)
      prompt="$1"
      shift
      ;;
  esac
done

if [ "$prompt" = "needs approval" ]; then
  decision=$(printf '{"session_id":"%s","tool_name":"Bash","tool_input":{"command":"echo from-claude"}}' "$session_id" | "$CQ_BIN" hook claude)
  echo "$decision" | grep -q '"permissionDecision":"deny"' && exit 1
fi

printf 'claude:%s\n' "$prompt"
"#,
    );

    let output = env
        .command()
        .env("CQ_CLAUDE_BIN", &fake_claude)
        .args([
            "start",
            "needs approval",
            "--name",
            "claude-task",
            "--backend",
            "claude",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let pending = wait_for_pending(&env, "claude-task", None, Some(&fake_claude));
    assert!(pending.contains("Bash"), "{pending}");

    let approve = env
        .command()
        .env("CQ_CLAUDE_BIN", &fake_claude)
        .args(["approve", "all", "--session", "claude-task"])
        .output()
        .unwrap();
    assert!(approve.status.success(), "{approve:?}");

    let wait = env
        .command()
        .env("CQ_CLAUDE_BIN", &fake_claude)
        .args(["wait", "claude-task"])
        .output()
        .unwrap();
    assert!(wait.status.success(), "{wait:?}");
    assert!(String::from_utf8_lossy(&wait.stdout).contains("claude:needs approval"));

    let conn = Connection::open(&env.db_path).unwrap();
    let backend: String = conn
        .query_row(
            "SELECT agent_backend FROM sessions WHERE name = 'claude-task' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(backend, "claude");
}

#[test]
fn start_keeps_parent_alive_for_backend_process() {
    let env = TestEnv::new(None);
    let fake_claude = env.install_script(
        "claude",
        r#"#!/bin/sh
initial_ppid=$(ps -o ppid= -p $$ | tr -d ' ')
session_id=""
prompt=""
while [ $# -gt 0 ]; do
  case "$1" in
    -p)
      shift
      ;;
    --session-id)
      session_id="$2"
      shift 2
      ;;
    --settings|--permission-mode)
      shift 2
      ;;
    --dangerously-skip-permissions)
      shift
      ;;
    *)
      prompt="$1"
      shift
      ;;
  esac
done

sleep 1
current_ppid=$(ps -o ppid= -p $$ | tr -d ' ')
if [ "$current_ppid" != "$initial_ppid" ]; then
  echo "parent changed from $initial_ppid to $current_ppid" >&2
  exit 9
fi

decision=$(printf '{"session_id":"%s","tool_name":"Bash","tool_input":{"command":"echo parent-ok"}}' "$session_id" | "$CQ_BIN" hook claude)
echo "$decision" | grep -q '"permissionDecision":"deny"' && exit 1

printf 'claude:%s\n' "$prompt"
"#,
    );

    let output = env
        .command()
        .env("CQ_CLAUDE_BIN", &fake_claude)
        .args([
            "start",
            "wait for parent",
            "--name",
            "parent-check",
            "--backend",
            "claude",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let pending = wait_for_pending(&env, "parent-check", None, Some(&fake_claude));
    assert!(pending.contains("parent-check"), "{pending}");
}

#[test]
fn start_updates_session_status_after_backend_exits() {
    let env = TestEnv::new(None);
    let fake_claude = env.install_script(
        "claude",
        r#"#!/bin/sh
prompt=""
while [ $# -gt 0 ]; do
  case "$1" in
    -p)
      shift
      ;;
    --session-id|--settings|--permission-mode)
      shift 2
      ;;
    --dangerously-skip-permissions)
      shift
      ;;
    *)
      prompt="$1"
      shift
      ;;
  esac
done

sleep 1
printf 'claude:%s\n' "$prompt"
"#,
    );

    let output = env
        .command()
        .env("CQ_CLAUDE_BIN", &fake_claude)
        .args([
            "start",
            "finishes later",
            "--name",
            "status-check",
            "--backend",
            "claude",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    thread::sleep(Duration::from_secs(2));

    let conn = Connection::open(&env.db_path).unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM sessions WHERE name = 'status-check' ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "completed");
}

#[test]
fn pi_session_model_from_config_is_passed_to_backend() {
    let env = TestEnv::new(Some("pi"));
    fs::write(
        env.project_dir.path().join(".cq").join("config.json"),
        r#"{"default_backend":"pi","timeout":5,"poll_interval":0.05,"backends":{"pi":{"model":"openai/gpt-5.4"}},"policies":[],"supervisor":{"enabled":false}}"#,
    )
    .unwrap();

    let fake_pi = env.install_script(
        "pi",
        r#"#!/bin/sh
model=""
prompt=""
while [ $# -gt 0 ]; do
  case "$1" in
    --session|--extension)
      shift 2
      ;;
    --model)
      model="$2"
      shift 2
      ;;
    --print|--no-extensions)
      shift
      ;;
    *)
      prompt="$1"
      shift
      ;;
  esac
done

if [ "$model" != "openai/gpt-5.4" ]; then
  echo "expected model openai/gpt-5.4, got '$model'" >&2
  exit 7
fi

printf 'pi:%s\n' "$prompt"
"#,
    );

    let output = env
        .command()
        .env("CQ_PI_BIN", &fake_pi)
        .args(["start", "hello", "--name", "pi-model"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let wait = env
        .command()
        .env("CQ_PI_BIN", &fake_pi)
        .args(["wait", "pi-model"])
        .output()
        .unwrap();
    assert!(wait.status.success(), "{wait:?}");
    assert!(String::from_utf8_lossy(&wait.stdout).contains("pi:hello"));
}

#[test]
fn pi_supervisor_uses_pi_backend_for_approval() {
    let env = TestEnv::new(Some("pi"));
    fs::write(
        env.project_dir.path().join(".cq").join("config.json"),
        r#"{"default_backend":"pi","timeout":5,"poll_interval":0.05,"policies":[],"supervisor":{"enabled":true,"backends":{"pi":{"model":"openai/gpt-5.4"}}}}"#,
    )
    .unwrap();

    let fake_pi = env.install_script(
        "pi",
        r#"#!/bin/sh
mode="session"
session_file=""
prompt=""
while [ $# -gt 0 ]; do
  case "$1" in
    --session)
      session_file="$2"
      shift 2
      ;;
    --extension)
      shift 2
      ;;
    --model)
      shift 2
      ;;
    --print)
      shift
      ;;
    --no-extensions)
      shift
      ;;
    --no-session|--no-tools|--no-skills)
      mode="supervisor"
      shift
      ;;
    *)
      prompt="$1"
      shift
      ;;
  esac
done

if [ "$mode" = "supervisor" ]; then
  printf '{"decision":"approve","reason":"approved by pi supervisor"}\n'
  exit 0
fi

mkdir -p "$(dirname "$session_file")"
printf '%s\n' "$prompt" >> "$session_file"

if [ "$prompt" = "needs supervisor" ]; then
  decision=$(printf '{"toolName":"bash","input":{"command":"echo from-pi"}}' | "$CQ_BIN" hook pi)
  echo "$decision" | grep -q '"decision":"deny"' && exit 1
fi

printf 'pi:%s\n' "$prompt"
"#,
    );

    let output = env
        .command()
        .env("CQ_PI_BIN", &fake_pi)
        .args(["start", "needs supervisor", "--name", "pi-supervised"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    thread::sleep(Duration::from_millis(250));

    let pending = env.command().args(["pending"]).output().unwrap();
    assert!(pending.status.success(), "{pending:?}");
    let pending_stdout = String::from_utf8_lossy(&pending.stdout);
    assert!(
        !pending_stdout.contains("pi-supervised"),
        "supervisor should have auto-approved via pi backend: {pending_stdout}"
    );

    let wait = env
        .command()
        .env("CQ_PI_BIN", &fake_pi)
        .args(["wait", "pi-supervised"])
        .output()
        .unwrap();
    assert!(wait.status.success(), "{wait:?}");
    assert!(
        String::from_utf8_lossy(&wait.stdout).contains("pi:needs supervisor"),
        "{wait:?}"
    );
}

fn wait_for_pending(
    env: &TestEnv,
    session_name: &str,
    pi_bin: Option<&Path>,
    claude_bin: Option<&Path>,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut cmd = env.command();
        if let Some(pi_bin) = pi_bin {
            cmd.env("CQ_PI_BIN", pi_bin);
        }
        if let Some(claude_bin) = claude_bin {
            cmd.env("CQ_CLAUDE_BIN", claude_bin);
        }
        let output = cmd.args(["pending"]).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if stdout.contains(session_name) && stdout.contains("Bash") {
            return stdout;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for pending approval"
        );
        thread::sleep(Duration::from_millis(50));
    }
}
