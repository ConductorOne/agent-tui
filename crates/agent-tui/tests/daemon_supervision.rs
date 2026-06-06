//! Daemon supervision / lifecycle — the RFC's #1 Phase-3 adoption hazard.
//!
//! When agent-tui is the env-manager's PTY substrate, the daemon MUST die when
//! its owner (the envmgr process) exits, and it must take its PTY children down
//! with it — otherwise an envmgr crash leaks a daemon + PTY + harness ("daemon
//! death = PTY death = harness death"). These tests drive the real `agent-tui`
//! binary → real daemon → real PTY child, kill the owner, and assert the whole
//! tree is reaped. No mocks.
//!
//! Why this is real-binary-only: the reaping is a property of the daemon
//! *process* exiting (and unblocking its `spawn_blocking` PTY reader), which a
//! library test of the async fns can't observe.
//!
//! Determinism (cov-3 macOS lesson): every wait polls the real condition (PID
//! liveness / socket file / pid-file) with a timeout — no fixed sleeps tuned to
//! a fast runner. PID liveness uses `kill -0` via the shell (portable across
//! Linux + macOS without an extra dep).
//!
//! Gated `cfg(unix)`: owner/daemon/child are POSIX processes and the monitor +
//! group-reap are Unix paths.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_agent-tui")
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// True while `pid` names a live (non-reaped) process. `kill -0` via the shell
/// keeps this dependency-free and portable (Linux + macOS).
fn pid_alive(pid: u32) -> bool {
    Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("kill -0 {pid} 2>/dev/null"))
        .status()
        .is_ok_and(|s| s.success())
}

/// Poll `cond` every 20ms until it is true or `max` elapses; returns whether it
/// became true.
fn poll_until(max: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if cond() {
            return true;
        }
        if start.elapsed() > max {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// An isolated socket dir + state home, plus a dedicated **owner** process the
/// daemon is told to monitor. Mirrors how the env-manager would launch the
/// daemon: a real owner PID via `--monitor-parent`, so killing the owner
/// exercises the production supervision path.
struct Supervised {
    root: PathBuf,
    socket_dir: PathBuf,
    state_home: PathBuf,
    owner: Child,
    daemon: Option<Child>,
    /// PIDs we must not leak even if an assertion fails mid-test.
    extra_pids: Vec<u32>,
}

impl Supervised {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(format!("/tmp/at-sup-{}-{n}-{tag}", std::process::id()));
        let socket_dir = root.join("s");
        let state_home = root.join("x");
        std::fs::create_dir_all(&socket_dir).expect("mkdir socket dir");
        std::fs::create_dir_all(&state_home).expect("mkdir state home");
        // A long-lived owner we fully control. Killing + reaping it makes
        // `kill(owner, 0)` return ESRCH, which the daemon's monitor detects.
        let owner = Command::new("/bin/sleep")
            .arg("600")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn owner");
        Self {
            root,
            socket_dir,
            state_home,
            owner,
            daemon: None,
            extra_pids: Vec::new(),
        }
    }

    fn socket_file(&self) -> PathBuf {
        self.socket_dir.join("t.sock")
    }

    /// A client/daemon `agent-tui` command bound to this session. `monitor` opts
    /// the (possibly lazily-spawned) daemon into watching a PID.
    fn cmd(&self, args: &[&str], monitor: Option<u32>) -> Command {
        let mut c = Command::new(bin());
        c.arg("--socket-dir")
            .arg(&self.socket_dir)
            .arg("--session")
            .arg("t")
            .args(args)
            .env("XDG_STATE_HOME", &self.state_home)
            .env("AGENT_TUI_SOCKET_DIR", &self.socket_dir);
        if let Some(pid) = monitor {
            c.env("AGENT_TUI_MONITOR_PARENT_PID", pid.to_string());
        }
        c
    }

    /// Launch the daemon as a foreground subprocess bound to the owner via
    /// `--monitor-parent` (so we own its PID), then wait for it to bind. We poll
    /// the socket FILE — never a client command, which would lazily spawn a
    /// competing daemon and confound the test.
    fn start_daemon(&mut self) -> u32 {
        let owner_pid = self.owner.id();
        let child = self
            .cmd(
                &[
                    "daemon",
                    "run",
                    "--monitor-parent",
                    &owner_pid.to_string(),
                    "--idle-timeout-secs",
                    "0",
                ],
                None,
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn daemon");
        let pid = child.id();
        self.daemon = Some(child);
        assert!(
            poll_until(Duration::from_secs(15), || self.socket_file().exists()),
            "daemon never bound its socket {}",
            self.socket_file().display()
        );
        pid
    }

    /// Spawn a long-lived PTY child that records its own PID, returning that
    /// PID. The child `exec`s `sleep` so the recorded `$$` is the live process.
    fn spawn_pane_child(&mut self) -> u32 {
        let pidfile = self.root.join("child.pid");
        let _ = std::fs::remove_file(&pidfile);
        let script = format!("echo $$ > {}; exec sleep 1000", pidfile.display());
        let out = self
            .cmd(&["spawn", "--", "/bin/sh", "-c", &script], None)
            .output()
            .expect("run spawn");
        assert!(
            out.status.success(),
            "spawn failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let read_pid = || {
            std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
        };
        assert!(
            poll_until(Duration::from_secs(10), || read_pid().is_some()),
            "pane child never wrote its pid file"
        );
        let pid = read_pid().expect("child pid");
        self.extra_pids.push(pid);
        pid
    }

    /// Kill + reap the owner so `kill(owner, 0)` returns ESRCH (a zombie still
    /// answers `kill -0`, so we must `wait` it).
    fn kill_owner(&mut self) {
        let _ = self.owner.kill();
        let _ = self.owner.wait();
    }

    /// Poll the daemon child until it exits, returning whether it did within
    /// `max`. Uses `try_wait` (which also reaps it) rather than `kill -0`: the
    /// daemon is OUR child, so once it exits it is a zombie that still answers
    /// `kill -0` until reaped — `try_wait` is the correct liveness check for a
    /// direct child.
    fn wait_daemon_exit(&mut self, max: Duration) -> bool {
        let daemon = self.daemon.as_mut().expect("daemon started");
        let start = Instant::now();
        loop {
            match daemon.try_wait() {
                // Exited (reaped) — or we can't wait on it, which we also treat
                // as "gone" rather than spin.
                Ok(Some(_)) | Err(_) => return true,
                Ok(None) => {}
            }
            if start.elapsed() > max {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        // Best-effort: never leak processes or temp dirs even if the test panicked.
        let _ = self.owner.kill();
        let _ = self.owner.wait();
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
        for pid in &self.extra_pids {
            let _ = Command::new("/bin/sh")
                .arg("-c")
                .arg(format!("kill -9 {pid} 2>/dev/null"))
                .status();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
}

/// THE headline property: when the owner process exits, the daemon must exit
/// AND reap its PTY child — nothing orphaned. This is the RFC's #1 adoption
/// hazard; if `--monitor-parent` stops reaping (or the daemon hangs on its
/// blocking PTY reader), this fails because the daemon and/or child linger.
#[test]
fn monitor_parent_reaps_daemon_and_pty_child_on_owner_death() {
    let mut s = Supervised::new("ownerdeath");
    let daemon_pid = s.start_daemon();
    let child_pid = s.spawn_pane_child();

    // Sanity: everything is live before the owner dies.
    assert!(pid_alive(daemon_pid), "daemon should be alive pre-kill");
    assert!(pid_alive(child_pid), "pane child should be alive pre-kill");

    // Owner (envmgr stand-in) crashes.
    s.kill_owner();

    // Within a bounded window the daemon must exit and its PTY child must be
    // reaped — owner death tears down the whole tree, nothing leaked.
    assert!(
        s.wait_daemon_exit(Duration::from_secs(15)),
        "daemon {daemon_pid} did not exit after owner death (leak hazard)"
    );
    assert!(
        poll_until(Duration::from_secs(15), || !pid_alive(child_pid)),
        "PTY child {child_pid} leaked after owner death — daemon failed to reap it"
    );
}

/// `daemon shutdown --force` with a live pane must also exit the daemon process
/// (not hang on its blocking PTY reader) and reap the child; then the next
/// client command lazily respawns a fresh, working daemon.
#[test]
fn daemon_shutdown_reaps_pane_then_lazily_respawns() {
    let mut s = Supervised::new("shutdownrespawn");
    let daemon_pid = s.start_daemon();
    let child_pid = s.spawn_pane_child();
    assert!(pid_alive(daemon_pid) && pid_alive(child_pid));

    // Explicit shutdown. The ack returns, then the daemon must actually exit
    // (with a live pane this is exactly where a blocking-reader hang would
    // wedge the process) and reap the child.
    let out = s
        .cmd(&["daemon", "shutdown", "--force"], None)
        .output()
        .expect("run shutdown");
    assert!(
        out.status.success(),
        "shutdown rpc failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        s.wait_daemon_exit(Duration::from_secs(15)),
        "daemon {daemon_pid} did not exit after `daemon shutdown --force`"
    );
    assert!(
        poll_until(Duration::from_secs(15), || !pid_alive(child_pid)),
        "PTY child {child_pid} leaked after `daemon shutdown --force`"
    );
    assert!(
        poll_until(Duration::from_secs(10), || !s.socket_file().exists()),
        "socket file should be gone after shutdown"
    );

    // A subsequent command lazily respawns a fresh daemon (tied to our owner so
    // it cannot leak past this test) and succeeds with an empty registry.
    let owner_pid = s.owner.id();
    let status = s
        .cmd(&["daemon", "status"], Some(owner_pid))
        .output()
        .expect("run status");
    assert!(
        status.status.success(),
        "lazy respawn failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let data = json(&status);
    assert_eq!(
        data.get("data").and_then(|d| d.get("status")),
        Some(&Value::String("running".into())),
        "respawned daemon must be running: {data:?}"
    );
    assert_eq!(
        data.get("data").and_then(|d| d.get("panes")),
        Some(&Value::from(0)),
        "respawned daemon is fresh (no panes): {data:?}"
    );

    // Tidy up the respawned daemon.
    let _ = s.cmd(&["daemon", "shutdown", "--force"], None).output();
}
