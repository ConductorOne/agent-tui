//! End-to-end tests for the streaming output path — `watch` and
//! `tail --follow`.
//!
//! These verbs are the live-output half of the CLI: the daemon's
//! `handle_streaming_tail` polls the pane's output ring (~50ms) and emits
//! one `{type:"chunk"}` envelope per batch of new bytes, then a terminal
//! `{type:"eof"}` envelope when the child exits; the client's `stream`
//! reader consumes that multi-envelope sequence until EOF. None of it is
//! reachable from a library test — `watch_sugar` / `tail_follow` are
//! private async fns — so the only honest exercise is driving the real
//! binary → real daemon → real PTY child and asserting on the NDJSON the
//! pipeline actually emits. No mocks, no stubs.
//!
//! Coverage this reaches that `cargo test --workspace` otherwise misses:
//!  - `server.rs::handle_streaming_tail` (the streaming poll loop + the
//!    chunk/eof envelope shapes) — cold at ~53%
//!  - `commands.rs::watch_sugar` + `tail_follow` (the `--json` NDJSON
//!    branch) and `client.rs::stream` (the multi-envelope reader)
//!  - the `Tail { follow: true }` dispatch branch in both the daemon
//!    `handle_conn` streaming detection and the CLI `dispatch`
//!
//! Every wait is bounded (`run_bounded`) so a wedged stream fails fast
//! instead of hanging the suite. Output is asserted on content +
//! terminator, never on chunk *count* or timing — the poll loop may
//! coalesce fast output into a single chunk, which is correct behavior.
//!
//! Gated `cfg(unix)`: every child is a POSIX shell utility.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_agent-tui")
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// An isolated daemon keyed to a unique socket dir + state home, mirroring
/// the `run_verb` harness. `AGENT_TUI_MONITOR_PARENT_PID` ties the
/// lazily-spawned daemon to this test process so nothing is orphaned.
struct Harness {
    socket_dir: PathBuf,
    state_home: PathBuf,
}

impl Harness {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Short /tmp path: the daemon's Unix socket brushes the 108-byte
        // `sun_path` limit.
        let root = PathBuf::from(format!("/tmp/at-stream-{}-{n}-{tag}", std::process::id()));
        let socket_dir = root.join("s");
        let state_home = root.join("x");
        std::fs::create_dir_all(&socket_dir).expect("mkdir socket dir");
        std::fs::create_dir_all(&state_home).expect("mkdir state home");
        Self {
            socket_dir,
            state_home,
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(bin());
        c.arg("--socket-dir")
            .arg(&self.socket_dir)
            .arg("--session")
            .arg("t")
            .args(args)
            .env("XDG_STATE_HOME", &self.state_home)
            .env("AGENT_TUI_SOCKET_DIR", &self.socket_dir)
            .env(
                "AGENT_TUI_MONITOR_PARENT_PID",
                std::process::id().to_string(),
            );
        c
    }

    /// Run a command to completion, killing it (and failing) if it
    /// overruns `max`. Output is tiny, so a blocking read can't deadlock.
    fn run_bounded(&self, args: &[&str], max: Duration) -> Output {
        let mut child = self
            .cmd(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn agent-tui");
        let start = Instant::now();
        loop {
            if child.try_wait().expect("try_wait").is_some() {
                return child.wait_with_output().expect("wait_with_output");
            }
            if start.elapsed() > max {
                let _ = child.kill();
                let _ = child.wait();
                panic!("`agent-tui {args:?}` did not terminate within {max:?} — stream wedged");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Parse an NDJSON stdout stream into one JSON value per line.
    fn ndjson(out: &Output) -> Vec<Value> {
        let stdout = String::from_utf8_lossy(&out.stdout);
        stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str::<Value>(l)
                    .unwrap_or_else(|e| panic!("non-JSON streamed line {l:?}: {e}"))
            })
            .collect()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.cmd(&["daemon", "shutdown", "--force"]).output();
        let _ = std::fs::remove_dir_all(self.socket_dir.parent().unwrap_or(&self.socket_dir));
    }
}

/// `watch` = spawn + `tail --follow` + die, bundled. Under `--json` the
/// client emits one envelope per streamed chunk plus the terminal eof.
/// A child that echoes three lines and exits must produce chunk(s)
/// carrying all three lines, then exactly one `{type:"eof"}` terminator
/// with the child's exit code.
#[test]
fn watch_streams_child_output_then_eofs() {
    let h = Harness::new("watch");
    let out = h.run_bounded(
        &[
            "--json",
            "watch",
            "--",
            "/bin/sh",
            "-c",
            "for i in 1 2 3; do echo line-$i; done",
        ],
        Duration::from_secs(20),
    );
    let envs = Harness::ndjson(&out);
    assert!(
        !envs.is_empty(),
        "watch must stream at least one envelope; stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Concatenate every chunk's text; assert all three lines arrived.
    let mut streamed = String::new();
    let mut eof_count = 0;
    let mut saw_chunk = false;
    for (i, env) in envs.iter().enumerate() {
        let data = &env["data"];
        match data["type"].as_str() {
            Some("chunk") => {
                saw_chunk = true;
                if let Some(t) = data["text"].as_str() {
                    streamed.push_str(t);
                }
            }
            Some("eof") => {
                eof_count += 1;
                assert_eq!(
                    i,
                    envs.len() - 1,
                    "eof must be the final streamed envelope; got eof at {i} of {}",
                    envs.len()
                );
                assert_eq!(
                    data["exit_code"], 0,
                    "eof must carry the child's exit code: {data:?}"
                );
            }
            other => panic!("unexpected streamed envelope type {other:?}: {env:?}"),
        }
    }
    assert!(saw_chunk, "watch must emit at least one chunk: {envs:?}");
    assert_eq!(eof_count, 1, "exactly one eof terminator: {envs:?}");
    for line in ["line-1", "line-2", "line-3"] {
        assert!(
            streamed.contains(line),
            "streamed output must contain {line:?}; got {streamed:?}"
        );
    }
}

/// `tail --follow` driven directly against an already-spawned pane,
/// exercising the `Tail { follow: true }` dispatch branch and the client
/// `stream` reader without going through `watch`. The child has already
/// written its bytes by the time we follow; the stream must replay them
/// (from the output ring) as a chunk and then terminate on eof.
#[test]
fn tail_follow_replays_ring_then_eofs() {
    let h = Harness::new("tailf");
    // Spawn a short-lived child; its output persists in the pane's ring
    // buffer after exit (the pane lives until `die`).
    let spawn = h.run_bounded(
        &[
            "--json",
            "spawn",
            "--",
            "/bin/sh",
            "-c",
            "printf 'streamed-body\\n'",
        ],
        Duration::from_secs(15),
    );
    assert!(
        spawn.status.success(),
        "spawn failed: {} / {}",
        String::from_utf8_lossy(&spawn.stdout),
        String::from_utf8_lossy(&spawn.stderr)
    );

    let out = h.run_bounded(
        &["--json", "tail", "--follow", "--strip-ansi"],
        Duration::from_secs(20),
    );
    let envs = Harness::ndjson(&out);

    let mut streamed = String::new();
    let mut saw_eof = false;
    for env in &envs {
        let data = &env["data"];
        match data["type"].as_str() {
            Some("chunk") => {
                if let Some(t) = data["text"].as_str() {
                    streamed.push_str(t);
                }
            }
            Some("eof") => saw_eof = true,
            _ => {}
        }
    }
    assert!(
        streamed.contains("streamed-body"),
        "tail --follow must replay the ring buffer; got {streamed:?} (envs={envs:?})"
    );
    assert!(
        saw_eof,
        "tail --follow must terminate with an eof envelope: {envs:?}"
    );

    let _ = h.cmd(&["die"]).output();
}

/// G5: a `tail --follow` CLI process mirrors the child's fate as its own exit
/// status — even when a THIRD client kills the pane. A supervising process can
/// thus read 143 (SIGTERM → 128+15) straight off the wrapper's exit status.
#[test]
fn follow_cli_exit_status_mirrors_third_party_die() {
    let h = Harness::new("exitcode");
    // Spawn a SIGTERM-dying pane that persists in the daemon.
    let spawn = h.run_bounded(
        &["spawn", "--", "/bin/sleep", "1000"],
        Duration::from_secs(20),
    );
    assert!(
        spawn.status.success(),
        "spawn failed: {}",
        String::from_utf8_lossy(&spawn.stderr)
    );

    // Start a follower as a background subprocess.
    let mut follower = h
        .cmd(&["tail", "--follow"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn follower");
    // Give it a moment to connect + start following before the kill.
    std::thread::sleep(Duration::from_millis(400));

    // A THIRD client kills the pane with grace → SIGTERM → 143.
    let die = h.run_bounded(&["die", "--grace", "2000"], Duration::from_secs(20));
    assert!(
        die.status.success(),
        "die failed: {}",
        String::from_utf8_lossy(&die.stderr)
    );

    // The follower CLI must exit with the child's status (143), not its own 0.
    let start = Instant::now();
    let status = loop {
        if let Some(s) = follower.try_wait().expect("try_wait") {
            break s;
        }
        if start.elapsed() > Duration::from_secs(15) {
            let _ = follower.kill();
            let _ = follower.wait();
            panic!("follower did not exit after 3rd-party die");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        status.code(),
        Some(143),
        "tail --follow CLI must exit with the child's SIGTERM status 143"
    );
}

// ---- cov-6: streaming-verb exit-status MATRIX (gap #6, P1) -----------------
//
// The G5 mirroring property is that ALL THREE streaming CLI verbs
// (`watch`, `attach`, `tail --follow`) exit with a status that mirrors the
// child across the full code space — clean 0, non-zero N, SIGTERM→143,
// SIGKILL→137 — because each captures the streamed `eof.exit_code` and
// `process::exit`s with it (`commands.rs`: `tail_follow` / `attach_stream`
// directly, `watch_sugar` by delegating to `tail_follow`). The existing
// `follow_cli_exit_status_mirrors_third_party_die` pins only
// `tail --follow × 143`; this matrix pins every verb × every outcome so a
// regression in any cell (e.g. a verb that always exits 0, or drops the
// signal→128+sig mapping) is caught.
//
// Determinism: the child reaches each outcome by ITSELF — `exit N` for the
// clean/non-zero cells, `kill -<SIG> $$` for the signal cells — so the pane
// is terminal-retained before the follower attaches and the eof carries the
// remembered code with no timing race (the cov-3 macOS lesson: wait on the
// real condition, never a fixed sleep). Every wait is bounded by `run_bounded`.

/// (label, `/bin/sh -c` script the child runs, expected mirrored CLI exit).
const EXIT_MATRIX: &[(&str, &str, i32)] = &[
    ("clean-0", "exit 0", 0),
    ("nonzero-7", "exit 7", 7),
    ("sigterm-143", "kill -TERM $$", 143),
    ("sigkill-137", "kill -KILL $$", 137),
];

/// Spawn a pane whose child reaches `script`'s outcome immediately (so the
/// pane is terminal-retained), then run a follow-style `verb` against it and
/// assert the verb's CLI process exits with `expected` — i.e. it mirrored the
/// child's fate off the streamed `eof.exit_code`.
fn assert_follow_mirrors(tag: &str, verb: &[&str], script: &str, expected: i32) {
    let h = Harness::new(tag);
    let spawn = h.run_bounded(
        &["spawn", "--", "/bin/sh", "-c", script],
        Duration::from_secs(20),
    );
    assert!(
        spawn.status.success(),
        "{tag}: spawn failed: {}",
        String::from_utf8_lossy(&spawn.stderr)
    );
    let out = h.run_bounded(verb, Duration::from_secs(20));
    assert_eq!(
        out.status.code(),
        Some(expected),
        "{tag}: `{verb:?}` CLI must mirror the child's exit {expected}; got {:?}\nstdout={}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = h.cmd(&["die"]).output();
}

/// `watch -- <argv>` spawns the child itself then follows via `tail_follow`,
/// so its CLI status must mirror the child across the whole matrix.
#[test]
fn watch_cli_exit_status_mirrors_child_matrix() {
    for (label, script, expected) in EXIT_MATRIX {
        let h = Harness::new(&format!("watch-{label}"));
        let out = h.run_bounded(
            &["watch", "--", "/bin/sh", "-c", script],
            Duration::from_secs(20),
        );
        assert_eq!(
            out.status.code(),
            Some(*expected),
            "watch × {label}: CLI must mirror the child's exit {expected}; got {:?}\nstderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// `tail --follow` against an already-terminal pane must mirror the child's
/// fate across the whole matrix (the existing G5 test covers only 143 via a
/// 3rd-party die; this adds 0 / N / 137 and a self-inflicted 143).
#[test]
fn tail_follow_cli_exit_status_mirrors_child_matrix() {
    for (label, script, expected) in EXIT_MATRIX {
        assert_follow_mirrors(
            &format!("tailf-{label}"),
            &["tail", "--follow"],
            script,
            *expected,
        );
    }
}

/// `attach` shares the same `eof.exit_code` → `process::exit` path
/// (`attach_stream`); pin that it mirrors the child across the whole matrix
/// too, so a future de-sugar/divergence from `tail --follow` is caught.
#[test]
fn attach_cli_exit_status_mirrors_child_matrix() {
    for (label, script, expected) in EXIT_MATRIX {
        assert_follow_mirrors(
            &format!("attach-{label}"),
            &["attach", "--prelude", "none"],
            script,
            *expected,
        );
    }
}
