//! End-to-end tests for the `wait` verb's exit-code contract.
//!
//! PR #93 ratified these codes as an interface contract for embedding hosts:
//!
//!   0   — condition satisfied within `--max`
//!   124 — condition NOT satisfied within `--max` (mirrors GNU `timeout(1)`)
//!   2   — any other failure (bad args, dead pane, daemon unreachable, …)
//!
//! These tests exercise the real CLI binary so that the exit-code mapping
//! from the socket protocol layer down to `std::process::exit` is covered.
//! Unit tests that only pin a constant cannot catch a regression where an
//! error propagates through `?` to `main()` and exits 1 instead.
//!
//! Gated `cfg(unix)`: child processes are POSIX utilities.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_agent-tui")
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Harness {
    socket_dir: PathBuf,
}

impl Harness {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(format!("/tmp/at-wec-{}-{}-{n}", std::process::id(), tag));
        let socket_dir = root.join("s");
        std::fs::create_dir_all(&socket_dir).expect("mkdir socket dir");
        Self { socket_dir }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(bin());
        c.arg("--socket-dir")
            .arg(&self.socket_dir)
            .arg("--session")
            .arg("t")
            .args(args)
            .env("AGENT_TUI_SOCKET_DIR", &self.socket_dir)
            .env(
                "AGENT_TUI_MONITOR_PARENT_PID",
                std::process::id().to_string(),
            );
        c
    }

    fn run_bounded(&self, args: &[&str], max: Duration) -> std::process::ExitStatus {
        let mut child = self
            .cmd(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn agent-tui");
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait().expect("try_wait") {
                return status;
            }
            if start.elapsed() > max {
                let _ = child.kill();
                let _ = child.wait();
                panic!("`agent-tui {args:?}` did not terminate within {max:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn exit_code(&self, args: &[&str], max: Duration) -> i32 {
        self.run_bounded(args, max)
            .code()
            .expect("process exited via signal, not exit code")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self
            .cmd(&["daemon", "shutdown", "--force"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let root = self.socket_dir.parent().unwrap_or(&self.socket_dir);
        let _ = std::fs::remove_dir_all(root);
    }
}

/// `wait --since <far_future> --max <ms>` — the condition is never met,
/// so `--max` fires → daemon returns `WAIT_TIMEOUT` → CLI must exit 124.
///
/// This is the #93 primary contract (mirrors GNU `timeout(1)`): shell
/// callers branch "not settled yet" (124) vs "actually broken" (2) without
/// parsing the JSON envelope. Embedding hosts rely on this.
#[test]
fn wait_exit_124_on_timeout() {
    let h = Harness::new("t124");
    // Spawn a quiet process so the daemon has a live pane to wait on.
    h.exit_code(&["spawn", "--", "/bin/cat"], Duration::from_secs(5));
    let code = h.exit_code(
        &["wait", "--since", "999999", "--max", "200"],
        Duration::from_secs(5),
    );
    assert_eq!(
        code, 124,
        "wait timeout must exit 124 (GNU timeout(1) convention); got {code}"
    );
}

/// Spawn a pane that prints a known string; `wait --text` matches it → exit 0.
///
/// Uses `--text` rather than `--since 0` because `/bin/sh` with `printf`
/// produces output asynchronously; `--text` polls until the string appears
/// (within `--max`), so there is no race and no sleep required.
#[test]
fn wait_exit_0_on_success() {
    let h = Harness::new("t0");
    h.exit_code(
        &["spawn", "--", "/bin/sh", "-c", "printf 'WAIT_OK'"],
        Duration::from_secs(5),
    );
    let code = h.exit_code(
        &["wait", "--text", "WAIT_OK", "--max", "3000"],
        Duration::from_secs(5),
    );
    assert_eq!(code, 0, "wait condition matched must exit 0; got {code}");
}

/// `wait` with no mode flag — bad-args failure must exit 2.
///
/// This is the regression from v0.1.10: the "wait requires exactly one mode
/// flag" error propagated through `?` to `main()`, which exits 1 instead of
/// the documented exit 2 for any non-timeout failure.
///
/// An end-to-end validator observed `agent-tui wait --timeout 500` exiting
/// 1 (not 124). In that invocation `--timeout` is the global flag (unwired
/// per tracker.md P-UX6), no mode flag is given, so the command fails
/// immediately with a bad-args error — exit must be 2, not 1.
#[test]
fn wait_exit_2_on_bad_args() {
    let h = Harness::new("t2ba");
    let code = h.exit_code(
        &["wait"], // no mode flag
        Duration::from_secs(5),
    );
    assert_eq!(
        code, 2,
        "wait with no mode flag must exit 2 (bad-args failure); got {code}"
    );
}

/// `wait` with a valid condition but daemon unreachable (socket dir exists
/// but no daemon listening) — communication failure must exit 2, not 1.
///
/// Before the fix, `client::one_shot` returning `Err` propagated via `?`
/// to `main()`, which exits 1 instead of the documented 2.
///
/// Note: agent-tui lazily spawns a daemon when the socket is missing; use
/// a socket dir on a path that prevents daemon startup (read-only root dir
/// equivalent: a file where a directory is expected) so the spawn fails.
#[test]
fn wait_exit_2_on_daemon_unreachable() {
    // Use a socket dir under a non-existent parent so the daemon can't
    // create its socket and lazy spawn fails quickly.
    let mut bad_harness = Harness::new("t2du");
    // Replace socket_dir with a path whose parent doesn't exist, so the
    // daemon's socket bind fails. Reset AGENT_TUI_MONITOR_PARENT_PID to
    // avoid accidentally keeping a partial daemon alive.
    bad_harness.socket_dir = PathBuf::from("/tmp/at-wec-nonexistent-parent/missing/s");

    let mut c = Command::new(bin());
    c.arg("--socket-dir")
        .arg(&bad_harness.socket_dir)
        .arg("--session")
        .arg("t")
        .args(["wait", "--since", "0", "--max", "200"])
        .env("AGENT_TUI_SOCKET_DIR", &bad_harness.socket_dir)
        // Do NOT set AGENT_TUI_MONITOR_PARENT_PID so any lazily-spawned
        // daemon orphans exit on their own after idle timeout.
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let start = Instant::now();
    let mut child = c.spawn().expect("spawn");
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            let code = status.code().expect("signal exit");
            assert_eq!(
                code, 2,
                "wait with unreachable daemon must exit 2; got {code}"
            );
            return;
        }
        if start.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("wait with unreachable daemon did not terminate within 10s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
