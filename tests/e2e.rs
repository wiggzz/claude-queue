//! End-to-end tests for cq (Claude Queue).
//!
//! These tests require `claude` CLI on PATH and spin up real sessions.
//! They are ignored by default — run with:
//!   cargo test --test e2e -- --ignored --test-threads=1 --nocapture
//!
//! Set CQ_E2E_MODEL to override the model (default: haiku).

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

/// Test harness that creates an isolated CQ_HOME so cq's DB/config don't pollute the real user.
/// Uses CQ_HOME (not HOME) so that claude's auth credentials remain accessible.
struct TestEnv {
    work_dir: PathBuf,
    cq_home: PathBuf,
    project_dir: PathBuf,
    cq_bin: PathBuf,
}

impl TestEnv {
    fn new(test_name: &str) -> Self {
        let work_dir = env::temp_dir().join(format!("cq-e2e-{}-{}", test_name, std::process::id()));
        let _ = fs::remove_dir_all(&work_dir);
        fs::create_dir_all(&work_dir).unwrap();

        // CQ_HOME isolates cq's DB and logs without touching claude's auth
        let cq_home = work_dir.join("cq-home");
        fs::create_dir_all(&cq_home).unwrap();

        let project_dir = work_dir.join("project");
        fs::create_dir_all(project_dir.join(".cq")).unwrap();

        let model = env::var("CQ_E2E_MODEL").unwrap_or_else(|_| "haiku".into());
        fs::write(
            project_dir.join(".cq/config.json"),
            format!(r#"{{"model": "{}"}}"#, model),
        )
        .unwrap();

        // Find the cq binary
        let cq_bin = env::var("CQ_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let manifest_dir = env!("CARGO_MANIFEST_DIR");
                PathBuf::from(manifest_dir).join("target/debug/cq")
            });

        TestEnv {
            work_dir,
            cq_home,
            project_dir,
            cq_bin,
        }
    }

    fn cq(&self, args: &[&str]) -> CqResult {
        let output = Command::new(&self.cq_bin)
            .args(args)
            .env("CQ_HOME", &self.cq_home)
            .output()
            .expect("failed to run cq");
        CqResult {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            success: output.status.success(),
        }
    }

    fn push(&self, name: &str, prompt: &str) -> CqResult {
        self.cq(&[
            "push",
            name,
            prompt,
            "--cwd",
            self.project_dir.to_str().unwrap(),
        ])
    }

    fn interrupt(&self, name: &str, prompt: &str) -> CqResult {
        self.cq(&[
            "interrupt",
            name,
            prompt,
            "--cwd",
            self.project_dir.to_str().unwrap(),
        ])
    }

    fn push_with_cwd(&self, name: &str, prompt: &str, cwd: &str) -> CqResult {
        self.cq(&["push", name, prompt, "--cwd", cwd])
    }

    /// Wait for the latest session with this name to reach a terminal state.
    /// Returns the status of the most recent (latest) session.
    /// Waits until at least `min_count` sessions exist before checking.
    fn wait_session_ex(&self, name: &str, timeout: Duration, min_count: usize) -> String {
        let start = Instant::now();
        loop {
            let result = self.cq(&["list", "--session", name]);
            let sessions: Vec<(&str, &str)> = result
                .stdout
                .lines()
                .skip(1) // header
                .filter_map(|line| {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        Some((parts[0], parts[1]))
                    } else {
                        None
                    }
                })
                .collect();

            // Wait for at least min_count sessions, and the latest one to be terminal
            if sessions.len() >= min_count {
                let latest_status = sessions[0].1;
                if latest_status == "completed"
                    || latest_status == "failed"
                    || latest_status == "killed"
                {
                    return latest_status.to_string();
                }
            }

            if start.elapsed() > timeout {
                panic!(
                    "Timeout ({:?}) waiting for session '{}' (min_count={}).\nLast list output: {}",
                    timeout, name, min_count, result.stdout
                );
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn wait_session(&self, name: &str, timeout: Duration) -> String {
        self.wait_session_ex(name, timeout, 1)
    }

    /// Get the result of the most recent session with this name.
    fn get_result(&self, name: &str) -> String {
        let list = self.cq(&["list", "--session", name]);
        // list is ordered DESC by id, so first line after header is the most recent
        for line in list.stdout.lines().skip(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if !parts.is_empty() {
                let result = self.cq(&["result", parts[0]]);
                return result.stdout;
            }
        }
        panic!("No session found for name '{}'", name);
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        // Keep work_dir on failure for debugging; set CQ_E2E_KEEP=1 to always keep
        if env::var("CQ_E2E_KEEP").is_ok() {
            eprintln!("Keeping test dir: {}", self.work_dir.display());
        } else {
            let _ = fs::remove_dir_all(&self.work_dir);
        }
    }
}

struct CqResult {
    stdout: String,
    stderr: String,
    success: bool,
}

// ── Tests ─────────────────────────────────────────────────────────────
// These tests are #[ignore] because they require the `claude` CLI and API access.
// Run with: cargo test --test e2e -- --ignored --test-threads=1 --nocapture

#[test]
#[ignore] // requires claude CLI
fn test_basic_push_and_wait() {
    let env = TestEnv::new("basic");
    let name = "basic";

    let result = env.push(name, "What is 9+9? Reply with ONLY the number, nothing else.");
    assert!(result.success, "push failed: {}", result.stderr);
    assert!(
        result.stdout.contains("Started session"),
        "Expected 'Started session' in: {}",
        result.stdout
    );

    let status = env.wait_session(name, Duration::from_secs(60));
    assert_eq!(status, "completed", "session should complete successfully");

    let output = env.get_result(name);
    assert!(
        output.contains("18"),
        "Expected '18' in output, got: {}",
        output
    );
}

#[test]
#[ignore]
fn test_queue_and_resume() {
    let env = TestEnv::new("queue");
    let name = "queue";

    let result = env.push(
        name,
        "What is 5+5? Reply with ONLY the number, nothing else.",
    );
    assert!(result.success, "first push failed: {}", result.stderr);

    // Queue a follow-up immediately — should queue since first is still running
    let result = env.push(
        name,
        "Now multiply your previous answer by 3. Reply with ONLY the number, nothing else.",
    );
    assert!(result.success, "second push failed: {}", result.stderr);
    // Should be queued or resumed (depending on timing)
    assert!(
        result.stdout.contains("Queued") || result.stdout.contains("Resumed"),
        "Expected 'Queued' or 'Resumed' in: {}",
        result.stdout
    );

    // Wait for the resumed (2nd) session to complete.
    // The wrapper auto-delivers queued messages when the first session finishes.
    let status = env.wait_session_ex(name, Duration::from_secs(120), 2);
    assert_eq!(
        status, "completed",
        "resumed session should complete successfully"
    );

    let output = env.get_result(name);
    assert!(
        output.contains("30"),
        "Expected '30' in output (5+5=10, 10×3=30), got: {}",
        output
    );
}

#[test]
#[ignore]
fn test_interrupt() {
    let env = TestEnv::new("interrupt");
    let name = "interrupt";

    let result = env.push(
        name,
        "What is 2+2? Reply with ONLY the number, nothing else.",
    );
    assert!(result.success, "push failed: {}", result.stderr);

    // Interrupt immediately — launch() waits for PID to be set, so interrupt should work right away
    let result = env.interrupt(
        name,
        "What is 2+3? Reply with ONLY the number, nothing else.",
    );
    assert!(result.success, "interrupt failed: {}", result.stderr);
    assert!(
        result.stdout.contains("Interrupted and resumed"),
        "Expected 'Interrupted and resumed' in: {}",
        result.stdout
    );

    // Wait for the resumed (2nd) session to complete
    let status = env.wait_session_ex(name, Duration::from_secs(60), 2);
    assert_eq!(
        status, "completed",
        "interrupted+resumed session should complete"
    );

    let output = env.get_result(name);
    assert!(
        output.contains("5"),
        "Expected '5' in output (2+3=5), got: {}",
        output
    );
}

#[test]
#[ignore]
fn test_cwd_mismatch_warning() {
    let env = TestEnv::new("cwd-warn");
    let name = "cwd-warn";

    let result = env.push(
        name,
        "What is 1+1? Reply with ONLY the number, nothing else.",
    );
    assert!(result.success, "push failed: {}", result.stderr);

    env.wait_session(name, Duration::from_secs(60));

    // Push with a different cwd — should warn on stderr
    let result = env.push_with_cwd(
        name,
        "What is 2+2? Reply with ONLY the number.",
        "/tmp",
    );
    assert!(
        result.stderr.contains("warning") && result.stderr.contains("differs"),
        "Expected cwd mismatch warning in stderr, got: {}",
        result.stderr
    );
}

#[test]
#[ignore]
fn test_list_shows_status() {
    let env = TestEnv::new("list");
    let name = "list-test";

    let result = env.push(
        name,
        "What is 3+3? Reply with ONLY the number, nothing else.",
    );
    assert!(result.success, "push failed: {}", result.stderr);

    // Should show as running immediately after push returns (launch waits for PID)
    let list = env.cq(&["list", "--session", name]);
    assert!(
        list.stdout.contains("running"),
        "Expected 'running' in list output: {}",
        list.stdout
    );

    env.wait_session(name, Duration::from_secs(60));

    let list = env.cq(&["list", "--session", name]);
    assert!(
        list.stdout.contains("completed"),
        "Expected 'completed' in list output after finish: {}",
        list.stdout
    );
}
