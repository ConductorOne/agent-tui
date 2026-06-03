//! End-to-end tests for two cold, high-value paths:
//!
//!  1. `session gc` CLI (gap R5) — `gc.rs` is well unit-tested but the CLI
//!     wrapper `commands.rs::session_gc` was 0%. This plants real stale
//!     crash residue (sidecar + cast files) for a dead session, stands up
//!     a **real live daemon** for another, then drives the real
//!     `agent-tui session gc` and asserts the dead one is reaped while the
//!     **live one survives** — and that `--dry-run` deletes nothing.
//!
//!  2. `OutputRing` eviction / `lost_bytes` (gap R2-residual) — the 1 MiB
//!     per-pane ring's eviction + stale-cursor accounting was cold. This
//!     drives a real child that emits more than the cap, then `tail`s from
//!     a stale cursor and asserts `lost_bytes` reports the dropped prefix
//!     (rather than silently corrupting the stream), with the cursor
//!     invariant `lost_bytes + returned == total` holding.
//!
//! All real system: real CLI, real daemon, real PTY child — no mocks.
//! Bounded waits, `Drop` cleanup, `AGENT_TUI_MONITOR_PARENT_PID`; no
//! sleeps / wall-clock races.
//!
//! Gated `cfg(unix)`: POSIX children (`/bin/sh`, `dd`) + filesystem layout.

#![cfg(unix)]

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_agent-tui")
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Isolated daemon home with explicit per-call session control (the gc
/// test drives two sessions: a dead one and a live one).
struct Harness {
    root: PathBuf,
    socket_dir: PathBuf,
    state_home: PathBuf,
}

impl Harness {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(format!("/tmp/at-gc-{}-{n}-{tag}", std::process::id()));
        let socket_dir = root.join("s");
        let state_home = root.join("x");
        std::fs::create_dir_all(&socket_dir).unwrap();
        std::fs::create_dir_all(&state_home).unwrap();
        Self {
            root,
            socket_dir,
            state_home,
        }
    }

    /// Build an invocation against this harness for an explicit session.
    fn cmd(&self, session: &str, args: &[&str]) -> Command {
        let mut c = Command::new(bin());
        c.arg("--socket-dir")
            .arg(&self.socket_dir)
            .arg("--session")
            .arg(session)
            .args(args)
            .env("XDG_STATE_HOME", &self.state_home)
            .env("AGENT_TUI_SOCKET_DIR", &self.socket_dir)
            .env(
                "AGENT_TUI_MONITOR_PARENT_PID",
                std::process::id().to_string(),
            );
        c
    }

    /// Run a command, killing + failing if it overruns `max`. stdout and
    /// stderr are drained on dedicated threads so a large response (e.g. a
    /// ~1.4 MB base64 `tail` payload) can't deadlock the child against a
    /// full pipe buffer while we wait for it to exit.
    fn run_bounded(&self, session: &str, args: &[&str], max: Duration) -> Output {
        let mut child = self
            .cmd(session, args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn agent-tui");
        let mut so = child.stdout.take().expect("child stdout");
        let mut se = child.stderr.take().expect("child stderr");
        let out_h = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = so.read_to_end(&mut b);
            b
        });
        let err_h = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = se.read_to_end(&mut b);
            b
        });
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait().expect("try_wait") {
                let stdout = out_h.join().expect("stdout reader");
                let stderr = err_h.join().expect("stderr reader");
                return Output {
                    status,
                    stdout,
                    stderr,
                };
            }
            if start.elapsed() > max {
                let _ = child.kill();
                let _ = child.wait();
                panic!("`agent-tui {args:?}` did not terminate within {max:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn json(out: &Output) -> Value {
        let stdout = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!(
                "non-JSON stdout {stdout:?}: {e}; stderr={:?}",
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }

    fn touch(&self, rel: &str) {
        let p = self.socket_dir.join(rel);
        std::fs::write(&p, b"x").unwrap();
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Shut down any live daemon (the gc test starts one for "alive").
        let _ = self
            .cmd("alive", &["daemon", "shutdown", "--force"])
            .output();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// `session gc` over real crash residue: a dead session's sidecars + cast
/// dir are reaped, a live session's are skipped, and `--dry-run` deletes
/// nothing.
#[test]
fn session_gc_reaps_dead_skips_live_and_respects_dry_run() {
    let h = Harness::new("gc");

    // Dead "ghost": stale sidecar files (a .sock that nothing listens on,
    // so the liveness probe fails) + a cast dir — i.e. crash leftovers.
    h.touch("ghost.sock");
    h.touch("ghost.pid");
    let ghost_cast = h.state_home.join("agent-tui").join("ghost");
    std::fs::create_dir_all(&ghost_cast).unwrap();
    std::fs::write(ghost_cast.join("p1.cast"), b"cast").unwrap();

    // Live "alive": a real daemon, lazy-spawned by a real spawn whose
    // pane sleeps. Its socket is a bound listener, so the probe succeeds.
    let spawn = h.run_bounded(
        "alive",
        &["spawn", "--", "/bin/sh", "-c", "sleep 30"],
        Duration::from_secs(15),
    );
    assert!(
        spawn.status.success(),
        "live daemon spawn failed: {}",
        String::from_utf8_lossy(&spawn.stderr)
    );
    assert!(
        h.socket_dir.join("alive.sock").exists(),
        "live daemon should have bound alive.sock"
    );

    // Dry-run first: reports the dead session but deletes nothing.
    let dry = Harness::json(&h.run_bounded(
        "ghost",
        &["--json", "session", "gc", "--all", "--dry-run"],
        Duration::from_secs(15),
    ));
    let dry_pruned: Vec<&str> = dry["pruned"]
        .as_array()
        .expect("pruned array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        dry_pruned.contains(&"ghost"),
        "dry-run should report ghost as prunable: {dry:?}"
    );
    assert_eq!(dry["dry_run"], true);
    assert!(
        h.socket_dir.join("ghost.sock").exists(),
        "dry-run must NOT delete the dead session's files"
    );
    assert!(ghost_cast.exists(), "dry-run must NOT delete the cast dir");

    // Real run with --all: reap dead, keep live.
    let report = Harness::json(&h.run_bounded(
        "ghost",
        &["--json", "session", "gc", "--all"],
        Duration::from_secs(15),
    ));
    let pruned: Vec<&str> = report["pruned"]
        .as_array()
        .expect("pruned array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        pruned.contains(&"ghost"),
        "real gc must reap the dead session: {report:?}"
    );
    assert!(
        !pruned.contains(&"alive"),
        "real gc must NOT reap the live session: {report:?}"
    );
    assert!(
        report["skipped_alive"].as_u64().unwrap_or(0) >= 1,
        "the live session must be counted skipped_alive: {report:?}"
    );

    // Dead session's residue is gone; live session's sidecars survive.
    assert!(
        !h.socket_dir.join("ghost.sock").exists(),
        "ghost.sock reaped"
    );
    assert!(!h.socket_dir.join("ghost.pid").exists(), "ghost.pid reaped");
    assert!(!ghost_cast.exists(), "ghost cast dir reaped");
    assert!(
        h.socket_dir.join("alive.sock").exists(),
        "a live session's socket must survive gc"
    );
}

/// A child that writes more than the 1 MiB output ring forces eviction;
/// a `tail` from a stale cursor must report the dropped prefix in
/// `lost_bytes` (never silently corrupt the stream), with the accounting
/// invariant `lost_bytes + returned == total`. A follow-up tail from the
/// fresh cursor sees nothing new and no further loss.
#[test]
fn tail_reports_lost_bytes_after_output_ring_eviction() {
    let h = Harness::new("ring");

    // 2,000,000 bytes — comfortably over the 1 MiB ring cap, so the ring
    // evicts its oldest bytes. `dd` from /dev/zero is deterministic and
    // doesn't get rewritten by the PTY line discipline (NUL ≠ NL).
    let spawn = h.run_bounded(
        "ring",
        &[
            "spawn",
            "--",
            "/bin/sh",
            "-c",
            "dd if=/dev/zero bs=100000 count=20 2>/dev/null",
        ],
        Duration::from_secs(15),
    );
    assert!(
        spawn.status.success(),
        "spawn failed: {}",
        String::from_utf8_lossy(&spawn.stderr)
    );

    // Wait for the child to finish writing before we tail.
    let waited = h.run_bounded(
        "ring",
        &["--json", "wait", "--exit", "--max", "10000"],
        Duration::from_secs(15),
    );
    let waited = Harness::json(&waited);
    assert_eq!(waited["success"], true, "wait --exit failed: {waited:?}");

    // Raw tail from a stale cursor (0) — past the ring's tail edge.
    let tail = Harness::json(&h.run_bounded(
        "ring",
        &["--json", "tail", "--since", "0"],
        Duration::from_secs(15),
    ));
    let data = &tail["data"];
    let total = data["next_since"].as_u64().expect("next_since");
    let lost = data["lost_bytes"].as_u64().expect("lost_bytes");
    let returned = base64::engine::general_purpose::STANDARD
        .decode(data["bytes_b64"].as_str().expect("bytes_b64"))
        .expect("valid base64")
        .len() as u64;

    assert!(
        total >= 2_000_000,
        "total bytes observed ({total}) should be at least what dd wrote"
    );
    assert!(
        lost > 0,
        "tailing from a stale cursor after eviction must report lost_bytes>0; got {lost}"
    );
    assert!(
        returned < total,
        "the ring must hold less than the full stream after eviction; returned={returned} total={total}"
    );
    assert_eq!(
        lost + returned,
        total,
        "cursor accounting invariant: lost_bytes + returned == total ({lost} + {returned} != {total})"
    );

    // Tail from the fresh high-water mark: nothing new, nothing lost.
    let tail2 = Harness::json(&h.run_bounded(
        "ring",
        &["--json", "tail", "--since", &total.to_string()],
        Duration::from_secs(15),
    ));
    let d2 = &tail2["data"];
    assert_eq!(d2["next_since"].as_u64(), Some(total), "high-water stable");
    assert_eq!(d2["lost_bytes"].as_u64(), Some(0), "no loss at the tip");
    let returned2 = base64::engine::general_purpose::STANDARD
        .decode(d2["bytes_b64"].as_str().expect("bytes_b64"))
        .expect("valid base64")
        .len();
    assert_eq!(returned2, 0, "no new bytes past the high-water mark");

    let _ = h.cmd("ring", &["die"]).output();
}
