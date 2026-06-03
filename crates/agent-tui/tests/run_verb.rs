//! End-to-end tests for the `run` orchestration verb — the
//! "subprocess-as-data" pattern that is the headline agent-facing
//! workflow (`agent-tui run -- <argv>`).
//!
//! Why this lives here and drives the real binary: `run` is pure
//! client-side stitching in `commands.rs::run_orchestrate` —
//! `spawn --stdin <mode>` → `stdin` → `close-stdin` → `wait --exit` →
//! `tail --strip-ansi` → `die` — and none of those private async fns are
//! reachable from a library test. The only honest way to exercise the
//! whole chain is to invoke the built `agent-tui` binary as a subprocess
//! and let it lazy-spawn a **real daemon** that runs a **real PTY child**
//! (host `/bin/sh`, `/bin/cat`). No mocks, no stubs: the test asserts on
//! the JSON envelope the real pipeline emits.
//!
//! Coverage this reaches that `cargo test --workspace` otherwise misses:
//!  - `commands.rs::run_orchestrate` (was 0% — the verb is unreachable
//!    without driving the binary)
//!  - the `pty.rs` custom-stdin path (`spawn_with_custom_stdin`,
//!    `write_stdin_pipe`, `close_stdin_pipe`, the close-on-exec pipe and
//!    its EOF-to-child semantics) — the daemon's own tests only spawn
//!    default `Pty`-stdin children, so the `Pipe`/`Closed` modes were cold
//!  - `client.rs` lazy-spawn + the per-command read-timeout that scales
//!    with `--max` (the live side of the PR #21 `wait --exit` fix)
//!  - the `wait --exit` → `finish` exit-code surfacing and `tail`'s
//!    ring-buffer read on the real wire
//!
//! Gated `cfg(unix)`: every child is a POSIX shell utility.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;
use serde_json::Value;

/// Path to the binary this package builds. Cargo sets `CARGO_BIN_EXE_<name>`
/// for integration tests in the same package that declares the `[[bin]]`.
fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_agent-tui")
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// An isolated daemon keyed to a unique socket dir + state home. Each
/// test gets its own so daemons never collide and parallel test threads
/// stay hermetic.
struct Harness {
    socket_dir: PathBuf,
    state_home: PathBuf,
}

impl Harness {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Short path under /tmp: the daemon's Unix socket brushes against
        // `sun_path`'s 108-byte limit, so keep the root tiny.
        let root = PathBuf::from(format!("/tmp/at-run-{}-{n}-{tag}", std::process::id()));
        let socket_dir = root.join("s");
        let state_home = root.join("x");
        std::fs::create_dir_all(&socket_dir).expect("mkdir socket dir");
        std::fs::create_dir_all(&state_home).expect("mkdir state home");
        Self {
            socket_dir,
            state_home,
        }
    }

    /// Build an `agent-tui` invocation against this harness's daemon.
    /// Globals (`--socket-dir`, `--session`) go before the subcommand.
    /// `AGENT_TUI_MONITOR_PARENT_PID` ties the lazily-spawned daemon's
    /// lifetime to this test process, so a panic or hang can't orphan it.
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

    /// Run `agent-tui --json run ...` and parse the single JSON envelope
    /// it prints to stdout. Returns `(parsed_json, exit_code)`.
    fn run_json(&self, run_args: &[&str]) -> (Value, i32) {
        let mut args = vec!["--json", "run"];
        args.extend_from_slice(run_args);
        let out = self.cmd(&args).output().expect("spawn agent-tui run");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let value: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!("run did not emit JSON (err={e}); stdout={stdout:?} stderr={stderr:?}")
        });
        (value, out.status.code().unwrap_or(-1))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Best-effort: shut the daemon down so the socket is released
        // promptly. `--monitor-parent` is the backstop if this races.
        let _ = self.cmd(&["daemon", "shutdown", "--force"]).output();
        let _ = std::fs::remove_dir_all(self.socket_dir.parent().unwrap_or(&self.socket_dir));
    }
}

/// Happy path: a child that writes to stdout and exits 0. The whole
/// spawn → wait-exit → tail → die chain must yield the child's text and
/// a zero exit code.
#[test]
fn run_captures_child_stdout_and_zero_exit() {
    let h = Harness::new("ok");
    let (v, status) = h.run_json(&["--", "/bin/sh", "-c", "printf hello-from-child"]);
    assert_eq!(status, 0, "run should exit 0 on a clean child: {v:?}");
    assert_eq!(v["exit_code"], 0, "child exit_code in envelope: {v:?}");
    let stdout = v["stdout"].as_str().expect("stdout field");
    assert!(
        stdout.contains("hello-from-child"),
        "captured stdout should contain the child's output; got {stdout:?}"
    );
    assert_eq!(
        v["argv"][0], "/bin/sh",
        "envelope echoes the argv it ran: {v:?}"
    );
}

/// The cold path: `--stdin <text>` spawns the child with a real stdin
/// *pipe* (not the PTY), writes the bytes, then closes the pipe so the
/// child sees EOF. `cat` echoes whatever it read back to its PTY stdout.
/// This is the only end-to-end exercise of `spawn_with_custom_stdin` +
/// `write_stdin_pipe` + `close_stdin_pipe` + the close-on-exec pipe's
/// EOF semantics — without the EOF, `cat` would block forever and the
/// `--max` deadline would trip instead of a clean exit.
#[test]
fn run_feeds_stdin_pipe_then_eof_lets_cat_exit() {
    let h = Harness::new("stdin");
    let (v, status) = h.run_json(&[
        "--stdin",
        "piped-payload\\n",
        "--max",
        "10000",
        "--",
        "/bin/cat",
    ]);
    assert_eq!(status, 0, "cat must exit cleanly after stdin EOF: {v:?}");
    assert_eq!(v["exit_code"], 0, "cat exit_code: {v:?}");
    let stdout = v["stdout"].as_str().expect("stdout field");
    assert!(
        stdout.contains("piped-payload"),
        "cat should echo the piped stdin back; got {stdout:?}"
    );
}

/// A non-zero child exit must surface in the envelope AND propagate to
/// the CLI process's own exit status (agents branch on `$?`).
#[test]
fn run_propagates_nonzero_exit_code() {
    let h = Harness::new("exit");
    let (v, status) = h.run_json(&["--", "/bin/sh", "-c", "exit 7"]);
    assert_eq!(
        v["exit_code"], 7,
        "envelope must carry the child's code: {v:?}"
    );
    assert_eq!(status, 7, "CLI process must exit with the child's code");
}

/// `--env K=V` must reach the child's environment. Exercises
/// `parse_env_pairs` and env propagation through the spawn wire.
#[test]
fn run_passes_env_to_child() {
    let h = Harness::new("env");
    let (v, status) = h.run_json(&[
        "--env",
        "RUN_VERB_PROBE=visible",
        "--",
        "/bin/sh",
        "-c",
        "printf %s \"$RUN_VERB_PROBE\"",
    ]);
    assert_eq!(status, 0, "{v:?}");
    let stdout = v["stdout"].as_str().expect("stdout field");
    assert!(
        stdout.contains("visible"),
        "child should observe the injected env var; got {stdout:?}"
    );
}

/// `--raw` returns the child's bytes base64-encoded with escape
/// sequences intact, where the default (stripped) path would remove
/// them. Proves the raw vs strip-ansi branch end-to-end.
#[test]
fn run_raw_preserves_escape_sequences_as_base64() {
    let h = Harness::new("raw");
    // Emit `ESC [ 1 m h i ESC [ 0 m` — a bold "hi" then reset.
    let (v, status) = h.run_json(&[
        "--raw",
        "--",
        "/bin/sh",
        "-c",
        "printf '\\033[1mhi\\033[0m'",
    ]);
    assert_eq!(status, 0, "{v:?}");
    let b64 = v["stdout_b64"]
        .as_str()
        .expect("stdout_b64 field in raw mode");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .expect("raw payload must be valid base64");
    assert!(
        bytes.contains(&0x1b),
        "raw mode must keep the ESC byte; got {bytes:?}"
    );
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("hi"),
        "raw payload should still carry 'hi'; got {text:?}"
    );
}
