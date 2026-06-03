//! End-to-end tests for the `replay` and `edit` orchestration verbs —
//! the remaining cold half of gap R1.
//!
//! Both run through the real built binary; neither is reachable from a
//! library test (`replay_cast` / `render_replay_snapshot` / `edit_sugar`
//! are private async fns).
//!
//!  - `replay` is **fully offline**: it feeds a real `.cast` file through
//!    a fresh alacritty engine and renders a snapshot — no daemon, no
//!    PTY. The tests craft a minimal valid asciicast and assert the
//!    rendered text, plus the `--expect-snapshot` pass/mismatch branches
//!    (including the non-zero exit on mismatch).
//!  - `edit` drives `$EDITOR` end-to-end: it spawns the editor under a
//!    **real daemon + real PTY**, waits for it to exit, then returns the
//!    file's content. The test uses a **real non-interactive editor**
//!    (a tiny shell script that appends to its file argument) so the verb
//!    runs to completion without human interaction — a real editor
//!    process, not a mock.
//!
//! Coverage this reaches that `cargo test --workspace` otherwise misses:
//!  - `commands.rs::replay_cast` + `render_replay_snapshot` (both the
//!    plain-output and `--expect-snapshot` compare/exit branches)
//!  - `commands.rs::edit_sugar` (the `$EDITOR` → spawn → wait → read-back
//!    round-trip) driving `run_orchestrate` with a real editor child
//!
//! Gated `cfg(unix)`: `edit` spawns a POSIX shell-script editor.

#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_agent-tui")
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Unique per-test scratch root under /tmp.
fn tmp_root(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = PathBuf::from(format!("/tmp/at-re-{}-{n}-{tag}", std::process::id()));
    std::fs::create_dir_all(&root).expect("mkdir scratch root");
    root
}

// --------------------------------------------------------------------------
// replay — fully offline (no daemon / no PTY)
// --------------------------------------------------------------------------

/// Write a minimal asciicast v2 file: a header line plus one `o` (output)
/// event carrying `payload`. Returns the cast path.
fn write_cast(root: &Path, payload: &str) -> PathBuf {
    let cast = root.join("session.cast");
    let mut f = std::fs::File::create(&cast).expect("create cast");
    // Header line (object) — `replay_cast` skips any non-`[` line.
    writeln!(f, "{}", json!({"version": 2, "width": 80, "height": 24})).unwrap();
    // One output event: [timestamp, "o", payload].
    writeln!(f, "{}", json!([0.01, "o", payload])).unwrap();
    cast
}

/// `agent-tui --json replay <cast>` renders the fed bytes into a snapshot
/// whose `text` carries the visible output.
#[test]
fn replay_renders_cast_output_text() {
    let root = tmp_root("replay-json");
    let cast = write_cast(&root, "hello-replay\r\n");

    let out = Command::new(bin())
        .args(["--json", "replay"])
        .arg(&cast)
        .output()
        .expect("run replay");
    assert!(
        out.status.success(),
        "replay should exit 0; stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let payload: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("replay JSON: {e}; {stdout}"));
    assert_eq!(payload["cols"], 80);
    assert_eq!(payload["rows"], 24);
    assert_eq!(
        payload["text"].as_str().expect("text"),
        "hello-replay",
        "replayed snapshot text should be the rendered row (trailing blanks trimmed)"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `--expect-snapshot` must pass when the expected JSON matches the
/// rendered payload, and exit non-zero (with a MISMATCH diagnostic) when
/// it doesn't. Covers both arms of the compare branch.
#[test]
fn replay_expect_snapshot_pass_and_mismatch() {
    let root = tmp_root("replay-expect");
    let cast = write_cast(&root, "expected-body\r\n");

    // Capture the canonical rendered payload.
    let rendered = Command::new(bin())
        .args(["--json", "replay"])
        .arg(&cast)
        .output()
        .expect("run replay --json");
    let payload_json = String::from_utf8_lossy(&rendered.stdout).trim().to_string();
    assert!(
        !payload_json.is_empty(),
        "replay --json must print a payload"
    );

    // PASS: expected == rendered.
    let expect_ok = root.join("expected.json");
    std::fs::write(&expect_ok, &payload_json).unwrap();
    let pass = Command::new(bin())
        .args(["replay"])
        .arg(&cast)
        .arg("--expect-snapshot")
        .arg(&expect_ok)
        .output()
        .expect("run replay --expect-snapshot (pass)");
    assert!(
        pass.status.success(),
        "matching expected-snapshot must exit 0; stderr={:?}",
        String::from_utf8_lossy(&pass.stderr)
    );
    assert!(
        String::from_utf8_lossy(&pass.stderr).contains("PASS"),
        "stderr should report PASS: {:?}",
        String::from_utf8_lossy(&pass.stderr)
    );

    // MISMATCH: tweak the expected text so it can't match.
    let mut wrong: Value = serde_json::from_str(&payload_json).unwrap();
    wrong["text"] = json!("totally-different");
    let expect_bad = root.join("wrong.json");
    std::fs::write(&expect_bad, serde_json::to_string(&wrong).unwrap()).unwrap();
    let fail = Command::new(bin())
        .args(["replay"])
        .arg(&cast)
        .arg("--expect-snapshot")
        .arg(&expect_bad)
        .output()
        .expect("run replay --expect-snapshot (mismatch)");
    assert!(
        !fail.status.success(),
        "mismatched expected-snapshot must exit non-zero"
    );
    assert!(
        String::from_utf8_lossy(&fail.stderr).contains("MISMATCH"),
        "stderr should report MISMATCH: {:?}",
        String::from_utf8_lossy(&fail.stderr)
    );

    let _ = std::fs::remove_dir_all(&root);
}

// --------------------------------------------------------------------------
// edit — real daemon + real PTY editor child
// --------------------------------------------------------------------------

/// Isolated daemon home for the `edit` round-trip (it lazy-spawns a
/// daemon to run the editor under a PTY).
struct EditHarness {
    root: PathBuf,
    socket_dir: PathBuf,
    state_home: PathBuf,
}

impl EditHarness {
    fn new(tag: &str) -> Self {
        let root = tmp_root(tag);
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

    /// Run a command, killing + failing if it overruns `max`.
    fn run_bounded(&self, args: &[&str], max: Duration) -> Output {
        let mut child = self
            .cmd(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
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
                panic!("`agent-tui {args:?}` did not terminate within {max:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for EditHarness {
    fn drop(&mut self) {
        let _ = self.cmd(&["daemon", "shutdown", "--force"]).output();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// `edit <file> --editor <fake>` runs a real editor process (a shell
/// script that appends to its file arg) under the daemon's PTY, then
/// returns the file's content. The appended text must round-trip to the
/// caller's stdout, proving the spawn → wait-exit → read-back chain.
#[test]
fn edit_runs_real_editor_and_returns_file_content() {
    let h = EditHarness::new("edit");

    // A real, non-interactive "editor": appends a marker line to its
    // first argument (the path `edit` passes it), then exits 0.
    let editor = h.root.join("fake-editor.sh");
    std::fs::write(
        &editor,
        "#!/bin/sh\nprintf 'appended-by-editor\\n' >> \"$1\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Seed a file with known initial content.
    let target = h.root.join("doc.txt");
    std::fs::write(&target, "original-line\n").unwrap();

    let out = h.run_bounded(
        &[
            "edit",
            target.to_str().unwrap(),
            "--editor",
            editor.to_str().unwrap(),
        ],
        Duration::from_secs(20),
    );
    assert!(
        out.status.success(),
        "edit should exit 0; stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `edit` prints the file's post-edit content to stdout.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("original-line"),
        "edit output should preserve the original content; got {stdout:?}"
    );
    assert!(
        stdout.contains("appended-by-editor"),
        "edit output should include the editor's change; got {stdout:?}"
    );

    // And the change is actually persisted on disk.
    let on_disk = std::fs::read_to_string(&target).unwrap();
    assert!(
        on_disk.contains("original-line") && on_disk.contains("appended-by-editor"),
        "edited file on disk should carry both lines; got {on_disk:?}"
    );
}
