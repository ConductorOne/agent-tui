//! Real-system test for the in-place daemon upgrade (Option-A re-exec):
//! a session must survive a daemon binary upgrade in place.
//!
//! This exercises the **real** daemon re-exec against the **real** built
//! `agent-tui` binary — no shim, no mock. It drives the daemon directly on the
//! host (the daemon owns a real PTY + a real long-lived child), which is the
//! most faithful way to assert the load-bearing guarantees of a same-PID
//! re-exec:
//!
//!   (a) the daemon's PID is UNCHANGED across `daemon upgrade` (proving the
//!       `execve` re-exec'd into the same PID, not a fork);
//!   (b) the pane's CHILD is UNCHANGED — same pid, still alive, and still
//!       parented by the (same-pid) daemon — i.e. no SIGHUP death, the
//!       session survived the swap;
//!   (c) the output stream is CONTINUOUS — the child's counter keeps advancing
//!       past where it was at upgrade time, with no restart (the `.cast`-
//!       replayed grid still shows the live stream);
//!   (d) a subsequent SIGKILL yields the FAITHFUL exit code (137) — proving the
//!       new daemon is still the child's parent and `waitpid` works, so
//!       137/143/N fidelity carries across the upgrade.
//!
//! It lives in the integration crate's real daemon-lifecycle lanes (it runs
//! under both `--features docker` and `--features bwrap`, the two CI
//! integration jobs). Unlike the sandboxed scenarios it needs no fixture
//! rootfs/container — the re-exec mechanism is a property of the host daemon
//! itself — so it is also runnable directly. L2 (viewer reconnect) and L3
//! (N panes at scale, rotated-cast replay) are out of U1 scope; see the PR.

#![cfg(all(unix, any(feature = "docker", feature = "bwrap")))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use agent_tui_integration::agent_tui_binary;
use serde_json::Value;

/// A long-lived child that announces its own PID then prints a monotonic
/// counter forever — the discriminating fixture: a counter reset (or a second
/// `START_PID=` line) would betray a child restart; a stalled counter would
/// betray a SIGHUP death.
const COUNTER_PROGRAM: &str =
    "echo START_PID=$$; i=0; while true; do echo line=$i; i=$((i+1)); sleep 0.1; done";

struct Daemon {
    bin: PathBuf,
    socket_dir: PathBuf,
    state_home: PathBuf,
    root: PathBuf,
}

impl Daemon {
    fn new() -> Self {
        let bin = agent_tui_binary().expect("agent-tui binary built");
        // Short unique root: daemon-internal socket paths brush sun_path's
        // 108-byte limit.
        let nonce = format!("{:x}", std::process::id() as u64 ^ rand_seed());
        let root = PathBuf::from(format!("/tmp/at-upg-{nonce}"));
        let socket_dir = root.join("s");
        let state_home = root.join("x");
        std::fs::create_dir_all(&socket_dir).unwrap();
        std::fs::create_dir_all(&state_home).unwrap();
        Self {
            bin,
            socket_dir,
            state_home,
            root,
        }
    }

    /// Run one `agent-tui` CLI call and parse its JSON response. The daemon is
    /// lazy-spawned by the first call; `AGENT_TUI_MONITOR_PARENT_PID` ties its
    /// lifetime (and the re-exec'd image's) to this test process.
    fn cli(&self, args: &[&str]) -> Value {
        let out = Command::new(&self.bin)
            .arg("--socket-dir")
            .arg(&self.socket_dir)
            .args(args)
            .env("XDG_STATE_HOME", &self.state_home)
            .env("AGENT_TUI_SOCKET_DIR", &self.socket_dir)
            .env("AGENT_TUI_ALLOWED_BINARIES", "*")
            .env(
                "AGENT_TUI_MONITOR_PARENT_PID",
                std::process::id().to_string(),
            )
            .output()
            .unwrap_or_else(|e| panic!("run agent-tui {args:?}: {e}"));
        let body = String::from_utf8_lossy(&out.stdout);
        let line = body.trim().lines().last().unwrap_or("");
        serde_json::from_str(line).unwrap_or_else(|e| {
            panic!(
                "parse agent-tui {args:?} response: {e}\nstdout={body:?}\nstderr={:?}",
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }

    /// Daemon pid from the `<session>.pid` sidecar (session = `default`).
    fn daemon_pid(&self) -> Option<i32> {
        std::fs::read_to_string(self.socket_dir.join("default.pid"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Best-effort reap so no daemon/child outlives the test.
        let _ = Command::new(&self.bin)
            .arg("--socket-dir")
            .arg(&self.socket_dir)
            .args(["daemon", "shutdown"])
            .env("XDG_STATE_HOME", &self.state_home)
            .output();
        if let Some(pid) = self.daemon_pid() {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Cheap process-local nonce without pulling in a dep (uuid/rand are
/// workspace deps but this keeps the test self-contained).
fn rand_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn data<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.get("data")?.get(key)
}

/// Concatenated tail text since byte 0 (rendered with ANSI stripped).
fn tail_text(d: &Daemon) -> String {
    let v = d.cli(&["tail", "--since", "0", "--strip-ansi"]);
    data(&v, "text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Highest `line=<N>` counter value visible in `text`, if any.
fn max_counter(text: &str) -> Option<u64> {
    text.lines()
        .filter_map(|l| l.strip_prefix("line=")?.trim().parse::<u64>().ok())
        .max()
}

fn ppid_of(pid: i32) -> Option<i32> {
    // /proc/<pid>/stat field 4 is ppid; the comm field (2) can contain
    // spaces/parens, so split on the last ')'.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(')')?.1;
    after.split_whitespace().nth(1)?.parse().ok()
}

#[test]
fn daemon_upgrade_preserves_session_pid_and_exit_fidelity() {
    let d = Daemon::new();

    // 1. Spawn a pane running a real long-lived counter child.
    let spawn = d.cli(&["spawn", "--", "sh", "-c", COUNTER_PROGRAM]);
    assert_eq!(
        spawn.get("success").and_then(Value::as_bool),
        Some(true),
        "spawn failed: {spawn}"
    );

    // Let the counter run and the daemon write its pidfile.
    std::thread::sleep(Duration::from_millis(1200));
    let daemon_pid_before = d
        .daemon_pid()
        .expect("daemon pidfile present before upgrade");

    let before = tail_text(&d);
    let child_pid: i32 = before
        .lines()
        .find_map(|l| l.strip_prefix("START_PID=")?.trim().parse().ok())
        .unwrap_or_else(|| panic!("no START_PID in pre-upgrade output:\n{before}"));
    let counter_before =
        max_counter(&before).unwrap_or_else(|| panic!("no counter pre-upgrade:\n{before}"));
    assert_eq!(
        ppid_of(child_pid),
        Some(daemon_pid_before),
        "child should be parented by the daemon before upgrade"
    );

    // 2. Trigger the in-place upgrade mid-stream.
    let up = d.cli(&["daemon", "upgrade"]);
    assert_eq!(
        up.get("success").and_then(Value::as_bool),
        Some(true),
        "daemon upgrade failed: {up}"
    );
    assert_eq!(
        data(&up, "status").and_then(Value::as_str),
        Some("upgrading"),
        "unexpected upgrade ack: {up}"
    );
    // The ack reports the same pid it's about to re-exec into.
    assert_eq!(
        data(&up, "pid").and_then(Value::as_i64),
        Some(i64::from(daemon_pid_before)),
        "upgrade ack pid should equal the running daemon pid"
    );

    // Give the deferred re-exec time to land + the adopted reader to resume.
    std::thread::sleep(Duration::from_millis(1500));

    // 3a. The daemon PID is UNCHANGED → same-PID re-exec (not a fork).
    let daemon_pid_after = d
        .daemon_pid()
        .expect("daemon pidfile present after upgrade");
    assert_eq!(
        daemon_pid_after, daemon_pid_before,
        "daemon pid must be identical across an in-place upgrade (same-PID re-exec)"
    );

    // 3b. The CHILD is UNCHANGED: same pid, alive, still parented by the daemon
    //     → the session survived, no SIGHUP death.
    assert!(
        Path::new(&format!("/proc/{child_pid}")).exists(),
        "child pid {child_pid} must still be alive after the upgrade"
    );
    assert_eq!(
        ppid_of(child_pid),
        Some(daemon_pid_after),
        "child must still be parented by the (same-pid) daemon after upgrade"
    );

    // 3c. The output stream is CONTINUOUS: the counter advanced past where it
    //     was, with no restart (a restart would re-print START_PID / reset to
    //     0). The fresh daemon's ring only holds post-adopt bytes, so every
    //     value here is strictly newer than the pre-upgrade max.
    let after = tail_text(&d);
    assert!(
        !after.contains("START_PID="),
        "post-upgrade stream must not show a child restart:\n{after}"
    );
    let counter_after =
        max_counter(&after).unwrap_or_else(|| panic!("no counter post-upgrade:\n{after}"));
    assert!(
        counter_after > counter_before,
        "counter must keep advancing across the upgrade (before={counter_before}, after={counter_after})"
    );

    // The .cast-replayed grid is live too — the rendered screen shows the
    // continuing counter, not a blank/garbage frame.
    let snap = d.cli(&["snapshot", "--mode", "text"]);
    let grid = data(&snap, "text").and_then(Value::as_str).unwrap_or("");
    assert!(
        grid.contains("line="),
        "replayed grid should render the live counter; got:\n{grid}"
    );

    // 3d. Faithful exit code through the adopted child: SIGKILL → 137. This
    //     only holds if the new daemon is still the parent and `waitpid`
    //     observes the signal death.
    let sig = d.cli(&["signal", "SIGKILL"]);
    assert_eq!(
        sig.get("success").and_then(Value::as_bool),
        Some(true),
        "signal SIGKILL failed: {sig}"
    );
    let waited = d.cli(&["wait", "--exit", "--max", "4000"]);
    assert_eq!(
        data(&waited, "exit_code").and_then(Value::as_i64),
        Some(137),
        "SIGKILL of the adopted child must surface as a faithful 137: {waited}"
    );
}
