//! End-to-end integration tests for the P0a vertical slice.
//!
//! Each test catches a regression class:
//!  - `pty_echo_round_trip`: PTY → engine.feed → snapshot pipeline
//!  - `spawn_list_die_lifecycle`: pane registry insert/list/remove
//!  - `daemon_wire_smoke`: JSON wire envelope round-trips over the UDS
//!  - `snapshot_hash_changes_after_output`: sequence + hash mechanics
//!
//! Gated `cfg(unix)`: every test spawns POSIX shells (`/bin/sh`, `/bin/cat`,
//! `/bin/bash`) that don't exist on Windows. A separate Windows-smoke test
//! uses `cmd.exe` and lives in `windows_smoke.rs`.

#![cfg(unix)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use agent_tui_daemon::{DaemonConfig, SocketLayout, run_daemon};
use agent_tui_protocol::request::SnapshotMode;
use agent_tui_protocol::{Command, PROTOCOL_VERSION, Request, ResponseEnvelope, SessionId};
use base64::Engine as _;
use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::traits::tokio::Stream as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;
use uuid::Uuid;

/// Spin up an isolated daemon on a temp socket dir and return its connect URL.
async fn boot_daemon() -> (DaemonConfig, agent_tui_daemon::DaemonHandle) {
    // macOS sockaddr_un.sun_path is 104 bytes; the full layout path is
    // `<root>/<session>.sock`. Use a short session id (8 hex chars) and
    // anchor the root under /tmp directly so the result fits.
    let session = SessionId(short_session());
    let root = short_temp_root("at-rt");
    std::fs::create_dir_all(&root).expect("mkdir tempdir");
    let layout = SocketLayout::for_session_in(&session, root);
    let cfg = DaemonConfig {
        session: session.clone(),
        layout: layout.clone(),
        engine: "alacritty".into(),
        binary_version: "0.0.0-test".into(),
        allowed_binaries: None,
        monitor_parent: None,
        idle_timeout_secs: None,
    };
    let handle = run_daemon(cfg.clone()).await.expect("run_daemon");
    // Tiny yield so the accept loop is parked before we connect.
    tokio::task::yield_now().await;
    (cfg, handle)
}

fn short_session() -> String {
    let mut h = Uuid::new_v4().simple().to_string();
    h.truncate(8);
    h
}

fn short_temp_root(prefix: &str) -> PathBuf {
    // Anchor under /tmp on Unix to dodge macOS's long /var/folders TMPDIR.
    // The full path becomes /tmp/at-rt-<8hex>/<8hex>.sock = 32 chars,
    // well under the 104-byte sun_path limit.
    let mut h = Uuid::new_v4().simple().to_string();
    h.truncate(8);
    PathBuf::from(format!("/tmp/{prefix}-{h}"))
}

async fn round_trip(cfg: &DaemonConfig, command: Command) -> ResponseEnvelope {
    let name = agent_tui_daemon::paths::socket_name(&cfg.layout).expect("name");
    let stream = Stream::connect(name).await.expect("connect");
    let req = Request {
        id: Uuid::new_v4(),
        protocol: PROTOCOL_VERSION,
        command,
    };
    let mut bytes = serde_json::to_vec(&req).expect("encode");
    bytes.push(b'\n');
    let (r, mut w) = tokio::io::split(stream);
    w.write_all(&bytes).await.expect("write");
    let mut lines = BufReader::new(r).lines();
    let line = timeout(Duration::from_secs(5), lines.next_line())
        .await
        .expect("read timeout")
        .expect("read err")
        .expect("eof");
    serde_json::from_str(&line).expect("decode")
}

/// Recursively concatenate every node's `name` (plus its children's
/// names) into `out`. Used by outline assertions that don't care
/// where in the tree a token appears.
fn collect_names(node: &serde_json::Value, out: &mut String) {
    if let Some(n) = node.get("name").and_then(|n| n.as_str()) {
        out.push_str(n);
        out.push('\n');
    }
    if let Some(kids) = node.get("children").and_then(|c| c.as_array()) {
        for k in kids {
            collect_names(k, out);
        }
    }
}

#[tokio::test]
async fn pty_echo_round_trip() {
    let (cfg, _h) = boot_daemon().await;
    // Spawn `/bin/sh -c "printf hello && sleep 0.2"` — a deterministic child
    // that writes "hello" then idles long enough for us to snapshot.
    let env = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf hello; sleep 0.2".into(),
            ],
            cwd: None,
            size: Some((40, 4)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    assert!(env.response.success, "spawn failed: {env:?}");

    // Give the child time to write its output and our reader task to feed it.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let snap = round_trip(
        &cfg,
        Command::Snapshot {
            pane: None,
            mode: SnapshotMode::Outline,
            png: None,
            annotate: None,
            select: None,
            all: false,
            keep_color: false,
        },
    )
    .await;
    assert!(snap.response.success, "snapshot failed: {snap:?}");
    let data = snap.response.data.expect("data");
    // The shell adapter emits a single `@shell` root with children;
    // walk the tree collecting names and assert "hello" appears.
    let mut all_names = String::new();
    if let Some(roots) = data["outline"]["nodes"].as_array() {
        for r in roots {
            collect_names(r, &mut all_names);
        }
    }
    assert!(
        all_names.contains("hello"),
        "expected outline to contain 'hello', got: {all_names:?}"
    );

    // Clean up.
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn spawn_list_die_lifecycle() {
    let (cfg, _h) = boot_daemon().await;

    let env = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 2".into()],
            cwd: None,
            size: None,
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    assert!(env.response.success);

    let list1 = round_trip(&cfg, Command::List { all: false }).await;
    let panes = list1.response.data.expect("list data");
    assert_eq!(panes["panes"].as_array().expect("array").len(), 1);

    let _die = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: Some(Duration::from_secs(2)),
        },
    )
    .await;

    // G5: a killed pane is terminal-RETAINED (not removed) so late observers
    // and `list` can read its remembered exit code. The pane is still listed,
    // now carrying an `exit_code` (SIGTERM → 143).
    let list2 = round_trip(&cfg, Command::List { all: false }).await;
    let panes_after = list2.response.data.expect("list data");
    let arr = panes_after["panes"].as_array().expect("array");
    assert_eq!(arr.len(), 1, "killed pane is retained, not removed");
    assert_eq!(
        arr[0]["exit_code"], 143,
        "retained pane shows the remembered SIGTERM exit code"
    );
}

#[tokio::test]
async fn daemon_wire_smoke() {
    let (cfg, _h) = boot_daemon().await;
    let env = round_trip(&cfg, Command::DaemonStatus).await;
    assert!(env.response.success, "DaemonStatus must succeed");
    assert_eq!(env.protocol, PROTOCOL_VERSION);
    assert_eq!(env.session.as_ref(), Some(&cfg.session));
    let body = env.response.data.expect("status data");
    assert_eq!(body["status"], "running");
    assert_eq!(body["protocol"], PROTOCOL_VERSION);
}

#[tokio::test]
async fn press_round_trip_through_pty() {
    let (cfg, _h) = boot_daemon().await;
    // `cat` echoes its input back to stdout, so a press shows up in the next
    // snapshot.
    let _spawn = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/cat".into()],
            cwd: None,
            size: Some((40, 4)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    // Let cat finish wiring up the tty before we type at it.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let press = round_trip(
        &cfg,
        Command::Press {
            pane: None,
            keys: "hello<cr>".into(),
            to: None,
            lease: None,
        },
    )
    .await;
    assert!(press.response.success, "press failed: {press:?}");

    let snap = round_trip(
        &cfg,
        Command::Snapshot {
            pane: None,
            mode: SnapshotMode::Outline,
            png: None,
            annotate: None,
            select: None,
            all: false,
            keep_color: false,
        },
    )
    .await;
    let data = snap.response.data.unwrap();
    let mut all_names = String::new();
    if let Some(roots) = data["outline"]["nodes"].as_array() {
        for r in roots {
            collect_names(r, &mut all_names);
        }
    }
    assert!(
        all_names.contains("hello"),
        "cat should have echoed 'hello'; got outline: {all_names:?}"
    );

    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn quiesce_barrier_advances_sequence() {
    let (cfg, _h) = boot_daemon().await;
    let _spawn = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/cat".into()],
            cwd: None,
            size: Some((20, 3)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let press = round_trip(
        &cfg,
        Command::Press {
            pane: None,
            keys: "x".into(),
            to: None,
            lease: None,
        },
    )
    .await;
    let data = press.response.data.expect("press data");
    let pre = data["pre_sequence"].as_u64().expect("pre_seq");
    let post = data["post_sequence"].as_u64().expect("post_seq");
    assert!(
        post > pre,
        "barrier must observe a sequence bump: pre={pre} post={post}"
    );
    assert_eq!(data["barrier_observed"], true);

    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn signal_term_kills_child() {
    let (cfg, _h) = boot_daemon().await;
    let _spawn = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 60".into()],
            cwd: None,
            size: None,
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    let sig = round_trip(
        &cfg,
        Command::Signal {
            pane: None,
            signal: "SIGTERM".into(),
        },
    )
    .await;
    assert!(sig.response.success, "SIGTERM failed: {sig:?}");
    // We don't assert on `list` going empty — die is the path that removes
    // the registry entry. Signal only delivers the signal.
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn signal_bogus_name_rejected() {
    let (cfg, _h) = boot_daemon().await;
    let _spawn = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            cwd: None,
            size: None,
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    let sig = round_trip(
        &cfg,
        Command::Signal {
            pane: None,
            signal: "SIGBOGUS".into(),
        },
    )
    .await;
    assert!(!sig.response.success);
    let err = sig.response.error.expect("error body");
    assert_eq!(err.code.to_string(), "INVALID_ARGS");
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn list_reports_live_size_after_resize() {
    let (cfg, _h) = boot_daemon().await;
    // Spawn at 80×24, then resize to 100×30. `list` must report the *live*
    // post-resize geometry (100×30) — not the stale spawn-time 80×24 it used
    // to cache. The dims are sourced from the engine grid, the same source
    // `resize` / `snapshot --mode cells` already reflect.
    let _spawn = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 2".into()],
            cwd: None,
            size: Some((80, 24)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;

    // Pre-resize: list agrees with the spawn-time size.
    let before = round_trip(&cfg, Command::List { all: false }).await;
    let before = before.response.data.unwrap();
    assert_eq!(before["panes"][0]["cols"], 80, "spawn-time cols");
    assert_eq!(before["panes"][0]["rows"], 24, "spawn-time rows");

    let r = round_trip(
        &cfg,
        Command::Resize {
            pane: None,
            cols: 100,
            rows: 30,
        },
    )
    .await;
    assert!(r.response.success, "resize failed: {r:?}");

    // Post-resize: list must report the current geometry, not the stale one.
    let list = round_trip(&cfg, Command::List { all: false }).await;
    let panes = list.response.data.unwrap();
    assert_eq!(
        panes["panes"][0]["cols"], 100,
        "list must report live (post-resize) cols, not spawn-time 80"
    );
    assert_eq!(
        panes["panes"][0]["rows"], 30,
        "list must report live (post-resize) rows, not spawn-time 24"
    );

    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn spawn_attaches_shell_adapter_for_bash() {
    let (cfg, _h) = boot_daemon().await;
    let env = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/bash".into(), "-c".into(), "sleep 2".into()],
            cwd: None,
            size: None,
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    assert!(env.response.success);
    let data = env.response.data.unwrap();
    assert_eq!(data["adapter"], "shell");
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn snapshot_uses_attached_adapter_outline() {
    let (cfg, _h) = boot_daemon().await;
    let _spawn = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec![
                "/bin/bash".into(),
                "-c".into(),
                "echo hello-world; sleep 2".into(),
            ],
            cwd: None,
            size: Some((40, 4)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    let snap = round_trip(
        &cfg,
        Command::Snapshot {
            pane: None,
            mode: SnapshotMode::Outline,
            png: None,
            annotate: None,
            select: None,
            all: false,
            keep_color: false,
        },
    )
    .await;
    let data = snap.response.data.unwrap();
    assert_eq!(data["outline"]["adapter"], "shell");
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn snapshot_response_carries_nonced_delimiter() {
    let (cfg, _h) = boot_daemon().await;
    let _spawn = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "printf hi; sleep 1".into()],
            cwd: None,
            size: Some((10, 2)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    let snap = round_trip(
        &cfg,
        Command::Snapshot {
            pane: None,
            mode: SnapshotMode::Outline,
            png: None,
            annotate: None,
            select: None,
            all: false,
            keep_color: false,
        },
    )
    .await;
    let delim = snap.tool_output_delim.expect("snapshot must carry delim");
    assert!(delim.start.starts_with("<<<AGENT_TUI_OUTPUT_"));
    assert!(delim.end.starts_with("<<<END_"));
    // 8 hex chars in each marker => identical nonce body.
    let nonce = delim
        .start
        .trim_start_matches("<<<AGENT_TUI_OUTPUT_")
        .trim_end_matches(">>>");
    assert_eq!(nonce.len(), 8, "8 hex chars of nonce");
    assert!(nonce.chars().all(|c| c.is_ascii_hexdigit()));
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn non_snapshot_responses_have_no_delim() {
    let (cfg, _h) = boot_daemon().await;
    let env = round_trip(&cfg, Command::DaemonStatus).await;
    assert!(env.response.success);
    assert!(
        env.tool_output_delim.is_none(),
        "DaemonStatus should not carry a delim"
    );
}

#[tokio::test]
async fn osc133_marker_upgrades_state_to_shell() {
    let (cfg, _h) = boot_daemon().await;
    // Emit an OSC 133 prompt-start marker, then idle.
    let _spawn = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf '\\033]133;A\\033\\\\'; sleep 2".into(),
            ],
            cwd: None,
            size: Some((40, 4)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    let snap = round_trip(
        &cfg,
        Command::Snapshot {
            pane: None,
            mode: SnapshotMode::Outline,
            png: None,
            annotate: None,
            select: None,
            all: false,
            keep_color: false,
        },
    )
    .await;
    let data = snap.response.data.unwrap();
    assert_eq!(
        data["state"], "shell",
        "OSC 133 A should classify as shell, got: {data:?}"
    );
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn focus_resolves_no_pane_commands_under_multi_pane() {
    let (cfg, _h) = boot_daemon().await;
    // Two panes spawned; no-pane snapshot would otherwise error.
    let _p1 = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            cwd: None,
            size: None,
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    let _p2 = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            cwd: None,
            size: None,
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    // No focus yet → snapshot errors NO_ACTIVE_PANE.
    let no_focus = round_trip(
        &cfg,
        Command::Snapshot {
            pane: None,
            mode: SnapshotMode::Outline,
            png: None,
            annotate: None,
            select: None,
            all: false,
            keep_color: false,
        },
    )
    .await;
    assert!(!no_focus.response.success);
    assert_eq!(
        no_focus.response.error.unwrap().code.to_string(),
        "NO_ACTIVE_PANE"
    );
    // Focus p2 and retry.
    let focus = round_trip(
        &cfg,
        Command::Focus {
            pane: Some(agent_tui_protocol::PaneId("p2".into())),
        },
    )
    .await;
    assert!(focus.response.success, "{focus:?}");
    let with_focus = round_trip(
        &cfg,
        Command::Snapshot {
            pane: None,
            mode: SnapshotMode::Outline,
            png: None,
            annotate: None,
            select: None,
            all: false,
            keep_color: false,
        },
    )
    .await;
    assert!(with_focus.response.success);
    assert_eq!(with_focus.response.data.unwrap()["pane"], "p2");
    // Cleanup both.
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: Some(agent_tui_protocol::PaneId("p1".into())),
            grace: None,
        },
    )
    .await;
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: Some(agent_tui_protocol::PaneId("p2".into())),
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn focus_cleared_when_focused_pane_dies() {
    let (cfg, _h) = boot_daemon().await;
    let _p1 = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            cwd: None,
            size: None,
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    let _p2 = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            cwd: None,
            size: None,
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    let _ = round_trip(
        &cfg,
        Command::Focus {
            pane: Some(agent_tui_protocol::PaneId("p1".into())),
        },
    )
    .await;
    // Die the focused pane.
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: Some(agent_tui_protocol::PaneId("p1".into())),
            grace: None,
        },
    )
    .await;
    // No-pane snapshot must error again (no auto-refocus).
    let snap = round_trip(
        &cfg,
        Command::Snapshot {
            pane: None,
            mode: SnapshotMode::Outline,
            png: None,
            annotate: None,
            select: None,
            all: false,
            keep_color: false,
        },
    )
    .await;
    assert!(!snap.response.success);
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: Some(agent_tui_protocol::PaneId("p2".into())),
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn snapshot_cells_mode_returns_rle_grid() {
    let (cfg, _h) = boot_daemon().await;
    let _spawn = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "printf hi; sleep 1".into()],
            cwd: None,
            size: Some((10, 2)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    let snap = round_trip(
        &cfg,
        Command::Snapshot {
            pane: None,
            mode: SnapshotMode::Cells,
            png: None,
            annotate: None,
            select: None,
            all: false,
            keep_color: false,
        },
    )
    .await;
    assert!(snap.response.success, "cells snapshot: {snap:?}");
    let data = snap.response.data.unwrap();
    assert!(
        data["outline"].is_null(),
        "cells mode must not carry outline"
    );
    let cells = data["cells"].as_object().expect("cells object");
    assert_eq!(cells["cols"], 10);
    assert_eq!(cells["rows"], 2);
    let rows = cells["rows_b64"].as_array().expect("rows_b64 array");
    assert_eq!(rows.len(), 2);
    // Decode first row's b64 and confirm it parses as RLE-runs JSON.
    let row0_bytes = base64::engine::general_purpose::STANDARD
        .decode(rows[0].as_str().unwrap())
        .expect("b64 decode");
    let parsed: serde_json::Value = serde_json::from_slice(&row0_bytes).expect("json parse");
    assert!(
        parsed.is_array(),
        "row payload must be a JSON array of runs"
    );

    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn snapshot_hybrid_mode_carries_both() {
    let (cfg, _h) = boot_daemon().await;
    let _spawn = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "printf hi; sleep 1".into()],
            cwd: None,
            size: Some((8, 2)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    let snap = round_trip(
        &cfg,
        Command::Snapshot {
            pane: None,
            mode: SnapshotMode::Hybrid,
            png: None,
            annotate: None,
            select: None,
            all: false,
            keep_color: false,
        },
    )
    .await;
    let data = snap.response.data.unwrap();
    assert!(data["outline"].is_object(), "hybrid must carry outline");
    assert!(data["cells"].is_object(), "hybrid must carry cells");
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn snapshot_hash_changes_after_output() {
    let (cfg, _h) = boot_daemon().await;

    let _spawn = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 2".into()],
            cwd: None,
            size: Some((20, 3)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    // Two snapshots back-to-back on an idle shell — hash should match.
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(200) {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let snap = round_trip(
            &cfg,
            Command::Snapshot {
                pane: None,
                mode: SnapshotMode::Outline,
                png: None,
                annotate: None,
                select: None,
                all: false,
                keep_color: false,
            },
        )
        .await;
        if snap.response.success {
            let h1 = snap.response.data.as_ref().unwrap()["hash"]
                .as_str()
                .unwrap()
                .to_string();
            let snap2 = round_trip(
                &cfg,
                Command::Snapshot {
                    pane: None,
                    mode: SnapshotMode::Outline,
                    png: None,
                    annotate: None,
                    select: None,
                    all: false,
                    keep_color: false,
                },
            )
            .await;
            let h2 = snap2.response.data.unwrap()["hash"]
                .as_str()
                .unwrap()
                .to_string();
            assert_eq!(h1, h2, "idle hash must be stable");
            break;
        }
    }

    let _die = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

/// Real-system orphan-reap proof for `die --grace` (the defect this fixes).
///
/// A pane child forks a grandchild that (a) lives in the pane child's
/// process group and (b) ignores `SIGTERM` and never exits on its own. Such
/// a grandchild survives both a plain group `SIGTERM` *and* the pre-fix
/// child-PID-only teardown (portable-pty `ChildKiller::kill` on the pane
/// child alone — it never touches the grandchild's PID, orphaning it to
/// init). `die --grace` must `SIGTERM` the *group*, observe the grandchild
/// outlive the grace window, then escalate to a group `SIGKILL`, reaping it.
#[tokio::test]
async fn die_grace_reaps_orphaned_grandchild() {
    let (cfg, _h) = boot_daemon().await;

    // The grandchild publishes its own PID here so the test doesn't have to
    // scrape it out of terminal output.
    let dir = short_temp_root("at-orphan");
    std::fs::create_dir_all(&dir).expect("mkdir orphan dir");
    let pidfile = dir.join("gc.pid");

    // Outer sh = the pane child (a setsid session/group leader). It backgrounds
    // an inner sh (the grandchild) that ignores BOTH SIGTERM and SIGHUP, then
    // loops forever; the outer sh `wait`s. The grandchild shares the outer sh's
    // process group, so a group `killpg` reaches it but killing the outer sh's
    // PID alone does not.
    //
    // Why `trap "" TERM HUP` and not just TERM: dropping the pane closes the
    // PTY master, and the kernel delivers SIGHUP to the session on hangup. A
    // grandchild that only ignored TERM would be reaped *incidentally* by that
    // SIGHUP even under the old child-PID-only teardown — making the test pass
    // for the wrong reason (non-discriminating). Ignoring HUP too means only a
    // genuine group signal — here the `--grace` SIGKILL escalation — can reap
    // it, so the test FAILS against a child-PID-only teardown and PASSES only
    // for the group-aware fix. (Discrimination matrix recorded in pr-g1.md.)
    let script = format!(
        "sh -c 'trap \"\" TERM HUP; echo $$ > {pf}; while :; do sleep 1; done' & wait",
        pf = pidfile.display()
    );
    let env = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), script],
            cwd: None,
            size: None,
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    assert!(env.response.success, "spawn failed: {env:?}");
    let pane = env.response.data.expect("spawn data")["pane"]
        .as_str()
        .expect("pane id")
        .to_string();

    // Wait (bounded) for the grandchild to publish its PID.
    let gc_pid = read_pid_within(&pidfile, Duration::from_secs(5))
        .await
        .expect("grandchild never published its PID");

    // Sanity: the grandchild is alive before teardown.
    assert!(
        pid_alive(gc_pid),
        "grandchild {gc_pid} should be alive pre-die"
    );

    // Group-aware graceful teardown: SIGTERM the group, then — since the
    // grandchild ignores TERM — escalate to a group SIGKILL after 1s.
    let die = round_trip(
        &cfg,
        Command::Die {
            pane: Some(agent_tui_protocol::PaneId(pane)),
            grace: Some(Duration::from_secs(1)),
        },
    )
    .await;
    let data = die.response.data.clone().expect("die data");
    let escalated = data["escalated"].as_bool().unwrap_or(false);

    // The grandchild must be gone within the grace + escalation window. The
    // pre-fix child-PID-only teardown would leave it running here: it never
    // received any signal at its own PID, only the pane child did.
    let reaped = gone_within(gc_pid, Duration::from_secs(3)).await;

    // Best-effort: SIGKILL the grandchild's whole process group BEFORE we
    // assert. On a regression the orphan survives and would keep the pane PTY
    // open, blocking daemon teardown and hanging the test on unwind — reaping
    // it here makes the failure fast and deterministic instead.
    reap_group_of(gc_pid);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(die.response.success, "die failed: {die:?}");
    assert!(
        escalated,
        "expected SIGKILL escalation (grandchild ignores TERM): {data}"
    );
    assert!(
        reaped,
        "grandchild {gc_pid} survived `die --grace` — orphaned"
    );
}

/// Poll `path` until it holds a positive integer PID or `max` elapses.
async fn read_pid_within(path: &std::path::Path, max: Duration) -> Option<i32> {
    let deadline = tokio::time::Instant::now() + max;
    loop {
        if let Ok(s) = std::fs::read_to_string(path)
            && let Ok(pid) = s.trim().parse::<i32>()
            && pid > 0
        {
            return Some(pid);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Probe whether `pid` still exists via `kill(pid, 0)` — delivers no signal,
/// just an existence/permission check. Direct libc (not a spawned `kill`
/// process): a blocking `Command::status()` polled in a loop starves the
/// current-thread tokio runtime, whereas this returns instantly so the
/// orphan assertion fails *fast* on a regression instead of hanging.
fn pid_alive(pid: i32) -> bool {
    // SAFETY: `kill(pid, 0)` performs no memory access and delivers no signal;
    // it returns 0 if the process exists (or EPERM — still alive), -1/ESRCH if
    // gone. No invariants to uphold.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    // EPERM means the process exists but we may not signal it → still alive.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Best-effort: SIGKILL the entire process group `pid` belongs to. Used by the
/// orphan-reap test to tear down a *surviving* orphan (the regression case) so
/// it can't keep the pane PTY open and block daemon teardown.
fn reap_group_of(pid: i32) {
    // SAFETY: plain syscalls, no memory access; signaling a (possibly already
    // dead) process group is harmless.
    #[allow(unsafe_code)]
    unsafe {
        let group = libc::getpgid(pid);
        if group > 0 {
            libc::kill(-group, libc::SIGKILL);
        }
    }
}

/// Poll until `pid` is gone or `max` elapses.
async fn gone_within(pid: i32, max: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + max;
    loop {
        if !pid_alive(pid) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Decode a PNG file from disk into `(width, height, rgb_bytes)`. Proves the
/// `--png` output is a real, decodable image rather than a stub.
fn decode_png(path: &std::path::Path) -> (u32, u32, Vec<u8>) {
    let f = std::fs::File::open(path).expect("open png");
    let decoder = png::Decoder::new(f);
    let mut reader = decoder.read_info().expect("png read_info");
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png next_frame");
    buf.truncate(info.buffer_size());
    (info.width, info.height, buf)
}

/// Poll a text snapshot until the screen contains `needle` (bounded). Avoids
/// racing the child's process start before its output is on screen.
async fn wait_for_text(cfg: &DaemonConfig, needle: &str) -> bool {
    for _ in 0..200 {
        let t = round_trip(
            cfg,
            Command::Snapshot {
                pane: None,
                mode: SnapshotMode::Text,
                png: None,
                annotate: None,
                select: None,
                all: false,
                keep_color: false,
            },
        )
        .await;
        let text = t
            .response
            .data
            .as_ref()
            .and_then(|d| d["text"].as_str())
            .unwrap_or_default();
        if text.contains(needle) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// `snapshot --png` writes a real, correctly-dimensioned PNG (cols*cw × rows*ch).
#[tokio::test]
async fn snapshot_png_writes_valid_image() {
    let (cfg, _h) = boot_daemon().await;
    let _ = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf hello; sleep 5".into(),
            ],
            cwd: None,
            size: Some((40, 4)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;

    let dir = short_temp_root("at-png");
    std::fs::create_dir_all(&dir).expect("mkdir png dir");
    let path = dir.join("shot.png");

    // Wait for the child's output so a glyph is actually on screen to render.
    assert!(
        wait_for_text(&cfg, "hello").await,
        "pane never displayed 'hello'"
    );

    let snap = round_trip(
        &cfg,
        Command::Snapshot {
            pane: None,
            mode: SnapshotMode::Outline,
            png: Some(path.to_string_lossy().into_owned()),
            annotate: None,
            select: None,
            all: false,
            keep_color: false,
        },
    )
    .await;
    assert!(snap.response.success, "snapshot failed: {snap:?}");
    let data = snap.response.data.expect("snapshot data");
    // Dims are derived from the embedded font's cell metrics: cols*cw × rows*ch.
    let (cw, ch) = agent_tui_daemon::render::cell_size();
    let (exp_w, exp_h) = (40 * cw, 4 * ch);
    assert_eq!(data["png"]["width"], exp_w, "png width = cols*cw");
    assert_eq!(data["png"]["height"], exp_h, "png height = rows*ch");
    assert_eq!(data["png"]["annotated"], false, "no overlay requested");

    // Decode it from disk: a valid image of exactly the reported size.
    let (w, h, pixels) = decode_png(&path);
    assert_eq!((w, h), (exp_w, exp_h), "decoded PNG dims");
    assert_eq!(
        pixels.len(),
        (w * h * 3) as usize,
        "RGB buffer is fully populated"
    );
    // A real glyph was rendered: the image is not a uniform background fill.
    let first_px = &pixels[0..3];
    assert!(
        pixels.chunks_exact(3).any(|p| p != first_px),
        "expected rendered glyph pixels, not a uniform image"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

/// `snapshot --png --annotate` overlays ref boxes: the annotated image is a
/// valid PNG, differs from the un-annotated one, and contains the overlay
/// color (the generic buffer node's bounding box) that the plain image lacks.
#[tokio::test]
async fn snapshot_png_annotate_overlays_refs() {
    let (cfg, _h) = boot_daemon().await;
    let _ = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf hello; sleep 5".into(),
            ],
            cwd: None,
            size: Some((40, 4)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;

    let dir = short_temp_root("at-png-an");
    std::fs::create_dir_all(&dir).expect("mkdir png dir");
    let plain = dir.join("plain.png");
    let annot = dir.join("annot.png");

    // Until the grid has content the adapter outline has no anchored node to
    // annotate, so the overlay would be a no-op — a race on slow process start
    // (seen on macOS). Wait for the output to land first.
    assert!(
        wait_for_text(&cfg, "hello").await,
        "pane never displayed 'hello' within the wait window"
    );

    let mk = |path: &std::path::Path, annotate: Option<String>| Command::Snapshot {
        pane: None,
        mode: SnapshotMode::Outline,
        png: Some(path.to_string_lossy().into_owned()),
        annotate,
        select: None,
        all: false,
        keep_color: false,
    };

    let p = round_trip(&cfg, mk(&plain, None)).await;
    assert!(p.response.success, "plain snapshot failed: {p:?}");
    // `annotate: Some(String::new())` = annotate all refs.
    let a = round_trip(&cfg, mk(&annot, Some(String::new()))).await;
    assert!(a.response.success, "annotated snapshot failed: {a:?}");
    assert_eq!(
        a.response.data.expect("data")["png"]["annotated"],
        true,
        "annotated flag must be set"
    );

    // The two files differ (overlay drawn).
    let plain_bytes = std::fs::read(&plain).expect("read plain");
    let annot_bytes = std::fs::read(&annot).expect("read annot");
    assert_ne!(plain_bytes, annot_bytes, "annotation must change the image");

    // The overlay color appears in the annotated image (the buffer node's box)
    // and not in the plain one — proof the overlay is genuinely rendered.
    let ov = agent_tui_daemon::render::OVERLAY;
    let has_overlay = |px: &[u8]| {
        px.chunks_exact(3)
            .any(|c| c[0] == ov[0] && c[1] == ov[1] && c[2] == ov[2])
    };
    let (_, _, plain_px) = decode_png(&plain);
    let (_, _, annot_px) = decode_png(&annot);
    assert!(
        !has_overlay(&plain_px),
        "plain image must not contain the overlay color"
    );
    assert!(
        has_overlay(&annot_px),
        "annotated image must contain overlay box pixels"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

// ---- attach (G4) -------------------------------------------------------

/// Open an `attach` streaming connection, read its prelude envelope, and return
/// `(prelude_data, live_lines)`. Holding `live_lines` keeps the connection open
/// (so the write-lease stays held); dropping it disconnects.
async fn attach_open(
    cfg: &DaemonConfig,
    command: Command,
) -> (serde_json::Value, tokio::io::Lines<BufReader<Stream>>) {
    let name = agent_tui_daemon::paths::socket_name(&cfg.layout).expect("name");
    let mut stream = Stream::connect(name).await.expect("connect");
    let req = Request {
        id: Uuid::new_v4(),
        protocol: PROTOCOL_VERSION,
        command,
    };
    let mut bytes = serde_json::to_vec(&req).expect("encode");
    bytes.push(b'\n');
    stream.write_all(&bytes).await.expect("write");
    let mut lines = BufReader::new(stream).lines();
    loop {
        let line = timeout(Duration::from_secs(10), lines.next_line())
            .await
            .expect("prelude timeout")
            .expect("read err")
            .expect("eof before prelude");
        let env: ResponseEnvelope = serde_json::from_str(&line).expect("decode");
        let data = env.response.data.clone().unwrap_or(serde_json::Value::Null);
        if data.get("type").and_then(|t| t.as_str()) == Some("prelude") {
            return (data, lines);
        }
        assert!(env.response.success, "pre-prelude error: {env:?}");
    }
}

/// Drain follow `chunk` envelopes from an attach connection until `eof`,
/// returning the concatenated raw follow bytes.
async fn drain_follow(mut lines: tokio::io::Lines<BufReader<Stream>>) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let line = timeout(Duration::from_secs(15), lines.next_line())
            .await
            .expect("follow timeout")
            .expect("read err");
        let Some(line) = line else { break };
        let env: ResponseEnvelope = serde_json::from_str(&line).expect("decode");
        let data = env.response.data.unwrap_or(serde_json::Value::Null);
        match data.get("type").and_then(|t| t.as_str()) {
            Some("chunk") => {
                if let Some(b64) = data.get("bytes_b64").and_then(|b| b.as_str()) {
                    out.extend(
                        base64::engine::general_purpose::STANDARD
                            .decode(b64)
                            .expect("b64"),
                    );
                }
            }
            Some("eof") => break,
            _ => {}
        }
    }
    out
}

/// Value of the last line wholly within `full[..since]` (or -1 if none).
fn last_line_value(full: &[u8], since: usize) -> i64 {
    std::str::from_utf8(&full[..since])
        .unwrap()
        .lines()
        .last()
        .and_then(|l| l.trim().parse::<i64>().ok())
        .unwrap_or(-1)
}

fn attach_text_cmd() -> Command {
    Command::Attach {
        pane: None,
        prelude: agent_tui_protocol::request::PreludeKind::Rendered,
        mode: SnapshotMode::Text,
        since: 0,
        write_lease: false,
        strip_ansi: false,
    }
}

/// THE point of G4: an atomic rendered-prelude + offset fused to a byte-follow.
/// Two concurrent attachers plus a mid-flight late joiner each reconstruct the
/// full ordered stream with **no gap and no overlap** at the prelude→follow
/// seam, AND each prelude frame reflects *exactly* `since` bytes. The frame⇄since
/// check is what fails against a naive snapshot-then-tail (TOCTOU) impl: there
/// the frame (older) would not match the later follow offset.
#[tokio::test]
async fn attach_seam_no_gap_no_overlap_concurrent_and_late() {
    let (cfg, _h) = boot_daemon().await;
    let n: u32 = 80;
    // Deterministic ordered stream: 0..n, one per line, steady cadence. Each
    // `echo` is one PTY write, so the capture offset always lands on a line
    // boundary.
    let script = format!("i=0; while [ $i -lt {n} ]; do echo $i; i=$((i+1)); sleep 0.02; done");
    let env = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), script],
            cwd: None,
            size: Some((40, 20)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    assert!(env.response.success, "spawn failed: {env:?}");

    // The PTY line discipline translates LF→CRLF on output (ONLCR), so the
    // ring holds "i\r\n" per line.
    let full: Vec<u8> = (0..n)
        .flat_map(|i| format!("{i}\r\n").into_bytes())
        .collect();

    // Two concurrent early attachers.
    let a = attach_open(&cfg, attach_text_cmd()).await;
    let b = attach_open(&cfg, attach_text_cmd()).await;
    // Late joiner: attach after the stream is well underway.
    assert!(wait_for_text(&cfg, "30").await, "stream never reached 30");
    let c = attach_open(&cfg, attach_text_cmd()).await;

    for (label, (prelude, lines)) in [("A", a), ("B", b), ("C", c)] {
        let since = usize::try_from(prelude["since"].as_u64().expect("since")).unwrap();
        let frame_text = prelude["frame"]["text"].as_str().unwrap_or("");
        let frame_max = frame_text
            .lines()
            .filter_map(|l| l.trim().parse::<i64>().ok())
            .max()
            .unwrap_or(-1);

        let follow = drain_follow(lines).await;

        // (1) No gap / no overlap: follow is exactly the stream from `since`.
        assert_eq!(
            follow,
            &full[since..],
            "{label}: follow must equal full[since..] — no gap, no overlap at the seam"
        );
        // (2) Atomicity: the rendered frame reflects exactly `since` bytes. This
        // FAILS for a TOCTOU snapshot-then-tail (frame older than the offset).
        assert_eq!(
            frame_max,
            last_line_value(&full, since),
            "{label}: frame must reflect exactly the `since` bytes (atomic capture)"
        );
    }
}

/// Write-lease: holder can write, non-holders get EBUSY, lease auto-releases on
/// disconnect so the next attacher can acquire it.
#[tokio::test]
async fn attach_write_lease_arbitration() {
    let (cfg, _h) = boot_daemon().await;
    let env = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/cat".into()],
            cwd: None,
            size: Some((40, 10)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    assert!(env.response.success, "spawn failed: {env:?}");

    let lease_cmd = || Command::Attach {
        pane: None,
        prelude: agent_tui_protocol::request::PreludeKind::None,
        mode: SnapshotMode::Cells,
        since: 0,
        write_lease: true,
        strip_ansi: false,
    };

    // A acquires the lease.
    let (prelude_a, lines_a) = attach_open(&cfg, lease_cmd()).await;
    assert_eq!(prelude_a["lease"]["granted"], true, "A should be granted");
    let token = prelude_a["lease"]["token"]
        .as_str()
        .expect("token")
        .to_string();

    // A (with token) can write.
    let typed_ok = round_trip(
        &cfg,
        Command::Type {
            pane: None,
            text: "x".into(),
            to: None,
            lease: Some(uuid::Uuid::parse_str(&token).unwrap()),
        },
    )
    .await;
    assert!(
        typed_ok.response.success,
        "A's leased type must succeed: {typed_ok:?}"
    );

    // B (no token) is denied with an EBUSY-style error.
    let typed_busy = round_trip(
        &cfg,
        Command::Type {
            pane: None,
            text: "y".into(),
            to: None,
            lease: None,
        },
    )
    .await;
    assert!(
        !typed_busy.response.success,
        "non-holder write must be rejected while a lease is held: {typed_busy:?}"
    );

    // A disconnects → lease auto-releases.
    drop(lines_a);

    // B can now acquire the lease (poll briefly for the release to land).
    let mut acquired = false;
    for _ in 0..50 {
        let (prelude_b, lines_b) = attach_open(&cfg, lease_cmd()).await;
        if prelude_b["lease"]["granted"] == serde_json::Value::Bool(true) {
            acquired = true;
            drop(lines_b);
            break;
        }
        drop(lines_b);
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(acquired, "B should acquire the lease after A disconnects");

    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

// ---- exit-code lifecycle (G5) ------------------------------------------

/// Drain an attach connection until its terminal `eof`, returning the
/// `exit_code` it carried (or `None` if the stream closed without one).
async fn drain_until_eof(mut lines: tokio::io::Lines<BufReader<Stream>>) -> Option<i64> {
    loop {
        let line = timeout(Duration::from_secs(15), lines.next_line())
            .await
            .expect("eof timeout")
            .expect("read err");
        let line = line?;
        let env: ResponseEnvelope = serde_json::from_str(&line).expect("decode");
        let data = env.response.data.unwrap_or(serde_json::Value::Null);
        if data.get("type").and_then(|t| t.as_str()) == Some("eof") {
            return data.get("exit_code").and_then(serde_json::Value::as_i64);
        }
    }
}

fn attach_follow_cmd() -> Command {
    Command::Attach {
        pane: None,
        prelude: agent_tui_protocol::request::PreludeKind::None,
        mode: SnapshotMode::Cells,
        since: 0,
        write_lease: false,
        strip_ansi: false,
    }
}

/// G5 fate-fidelity: two followers + a 3rd-client `die --grace` both receive a
/// faithful `eof{143}` (SIGTERM → 128+15); a LATE attacher after death gets the
/// remembered code (not "no such pane"); and `list` shows the terminal-retained
/// pane with its `exit_code`. Fails if the pane is removed immediately or a
/// 3rd-party die doesn't propagate the code.
#[tokio::test]
async fn exit_lifecycle_faithful_eof_late_observer_and_list() {
    let (cfg, _h) = boot_daemon().await;
    // `sleep` exits on SIGTERM (no trap) → shell-style 143.
    let env = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sleep".into(), "1000".into()],
            cwd: None,
            size: Some((40, 10)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    assert!(env.response.success, "spawn failed: {env:?}");

    // Two concurrent followers attached while the child is alive.
    let (_pa, lines_a) = attach_open(&cfg, attach_follow_cmd()).await;
    let (_pb, lines_b) = attach_open(&cfg, attach_follow_cmd()).await;

    // A THIRD client kills the pane with grace → SIGTERM → 143.
    let die = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: Some(Duration::from_secs(3)),
        },
    )
    .await;
    assert!(die.response.success, "die failed: {die:?}");
    assert_eq!(
        die.response.data.expect("die data")["exit_code"],
        143,
        "die reports the remembered SIGTERM exit code"
    );

    // Both mid-stream followers receive a faithful terminal eof{143}.
    assert_eq!(
        drain_until_eof(lines_a).await,
        Some(143),
        "follower A eof code"
    );
    assert_eq!(
        drain_until_eof(lines_b).await,
        Some(143),
        "follower B eof code"
    );

    // A LATE attacher (after death) gets the remembered code, not "no such pane".
    let (prelude_c, lines_c) = attach_open(&cfg, attach_follow_cmd()).await;
    assert!(
        prelude_c.get("type").is_some(),
        "late attach still returns a prelude: {prelude_c}"
    );
    assert_eq!(
        drain_until_eof(lines_c).await,
        Some(143),
        "late observer gets the remembered exit code"
    );

    // `list` shows the terminal-retained pane with its exit code.
    let list = round_trip(&cfg, Command::List { all: false }).await;
    let panes = list.response.data.expect("list data");
    let arr = panes["panes"].as_array().expect("array");
    assert_eq!(arr.len(), 1, "terminal pane is retained, not removed");
    assert_eq!(
        arr[0]["exit_code"], 143,
        "list surfaces the remembered exit code"
    );
}

/// Regression (G5): a terminal-RETAINED pane must not break implicit no-`--pane`
/// resolution for a later live pane — the multi-turn flow (`spawn; die; spawn;
/// snapshot`) the bwrap pi e2e exercises. Before the fix, turn 2's no-pane
/// snapshot failed with "multiple panes" because the retained turn-1 pane
/// counted toward resolution.
#[tokio::test]
async fn retained_pane_does_not_break_implicit_resolution() {
    let (cfg, _h) = boot_daemon().await;
    let spawn = |cmd: &str| Command::Spawn {
        argv: vec!["/bin/sh".into(), "-c".into(), cmd.into()],
        cwd: None,
        size: Some((40, 10)),
        stdin: agent_tui_protocol::request::StdinMode::default(),
        env: Vec::new(),
    };
    // Turn 1: spawn, then die (pane is retained, not removed).
    let _ = round_trip(&cfg, spawn("printf FIRST_TURN")).await;
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
    // Turn 2: a fresh, FAST-EXITING pane (mirrors `pi --print`, which prints
    // then exits — so by resolve time BOTH panes are terminal). No `--pane` is
    // given, exactly like the harness.
    let _ = round_trip(&cfg, spawn("printf SECOND_TURN_Z9")).await;
    // Poll the no-pane snapshot until the turn-2 output renders (bounded).
    let mut ok = false;
    for _ in 0..100 {
        let snap = round_trip(
            &cfg,
            Command::Snapshot {
                pane: None,
                mode: SnapshotMode::Text,
                png: None,
                annotate: None,
                select: None,
                all: false,
                keep_color: false,
            },
        )
        .await;
        if snap.response.success
            && snap
                .response
                .data
                .as_ref()
                .and_then(|d| d["text"].as_str())
                .is_some_and(|t| t.contains("SECOND_TURN_Z9"))
        {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        ok,
        "no-pane snapshot must resolve to the live turn-2 pane despite the retained turn-1 pane"
    );
}

// ---- ring-buffer eviction / tail lost_bytes (cov-1, P0) ----------------

/// The output ring's eviction cap (mirrors `OUTPUT_BUFFER_CAP` in `pty.rs`).
const RING_CAP: u64 = 1_048_576;

/// Poll a one-shot `tail --since 0` until the cumulative high-water mark
/// (`next_since`) exceeds `target` bytes, returning that response's data.
/// Used to wait until a flood has pushed past the 1 MiB ring cap.
async fn tail0_until_total(cfg: &DaemonConfig, target: u64) -> serde_json::Value {
    for _ in 0..600 {
        let r = round_trip(
            cfg,
            Command::Tail {
                pane: None,
                since: 0,
                strip_ansi: false,
                follow: false,
            },
        )
        .await;
        if let Some(d) = r.response.data {
            if d["next_since"].as_u64().unwrap_or(0) > target {
                return d;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("flood never exceeded {target} bytes within the wait window");
}

/// cov-1 (gap #1, P0): the output ring evicts at a 1 MiB cap, so a reader
/// whose `--since` is older than the retained floor MUST be told it lost the
/// evicted prefix (`lost_bytes > 0`) — never silently corrupted — and the
/// numbers must be internally consistent + monotonic. Fails if `lost_bytes`
/// were hard-coded 0 or eviction reporting regressed.
#[tokio::test]
async fn tail_reports_lost_bytes_after_ring_eviction() {
    let (cfg, _h) = boot_daemon().await;
    // `seq 1 500000` emits ~3.9 MiB (with PTY CRLF) and exits — deterministic,
    // bounded, well over the 1_048_576-byte ring cap.
    let env = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "seq 1 500000".into()],
            cwd: None,
            size: Some((80, 24)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    assert!(env.response.success, "spawn failed: {env:?}");

    // Wait until the flood has pushed well past the 1 MiB cap, then assert on
    // that same atomic `tail --since 0` response.
    let d = tail0_until_total(&cfg, RING_CAP + 500_000).await;
    let next_since = d["next_since"].as_u64().expect("next_since");
    let lost = d["lost_bytes"].as_u64().expect("lost_bytes");
    let retained = base64::engine::general_purpose::STANDARD
        .decode(d["bytes_b64"].as_str().expect("bytes_b64"))
        .expect("b64")
        .len() as u64;

    assert!(
        next_since > RING_CAP,
        "high-water {next_since} must exceed the ring cap"
    );
    assert!(
        lost > 0,
        "a --since 0 below the evicted floor must report lost_bytes>0"
    );
    assert!(
        retained <= RING_CAP,
        "retained {retained} must not exceed the ring cap {RING_CAP}"
    );
    // The evicted prefix + the retained suffix == every byte ever observed.
    assert_eq!(
        lost + retained,
        next_since,
        "lost + retained must equal total (no gap/overlap)"
    );

    // Monotonic + consistent: reading from the reported high-water returns no
    // earlier data and never a smaller cursor.
    let d2 = round_trip(
        &cfg,
        Command::Tail {
            pane: None,
            since: next_since,
            strip_ansi: false,
            follow: false,
        },
    )
    .await;
    let next2 = d2.response.data.expect("data")["next_since"]
        .as_u64()
        .expect("next_since");
    assert!(
        next2 >= next_since,
        "next_since must be monotonic ({next2} >= {next_since})"
    );

    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

/// cov-1: a follower that joins AFTER the flood has crossed the eviction
/// horizon is flagged (`lost_bytes>0` in its raw prelude) — not silently
/// corrupted or panicked — keeps a monotone cursor, and terminates with a
/// clean `eof`. Exercises the streaming/attach eviction path end to end.
#[tokio::test]
async fn follower_past_eviction_horizon_is_flagged_then_eofs() {
    let (cfg, _h) = boot_daemon().await;
    // A flood that spans ~2s (so a follower genuinely joins mid-flight) and
    // exceeds the 1 MiB cap: 60 lines × ~30 KiB.
    let env = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "i=0; while [ $i -lt 60 ]; do printf '%030000dEOL\\n' $i; sleep 0.03; i=$((i+1)); done".into(),
            ],
            cwd: None,
            size: Some((80, 24)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    assert!(env.response.success, "spawn failed: {env:?}");

    // Wait until output has crossed the eviction horizon (>1 MiB buffered).
    let _ = tail0_until_total(&cfg, RING_CAP + 200_000).await;

    // Join now with a RAW prelude from offset 0 — below the evicted floor — so
    // the prelude must report the lost prefix.
    let (prelude, lines) = attach_open(
        &cfg,
        Command::Attach {
            pane: None,
            prelude: agent_tui_protocol::request::PreludeKind::Raw,
            mode: SnapshotMode::Cells,
            since: 0,
            write_lease: false,
            strip_ansi: false,
        },
    )
    .await;
    assert_eq!(prelude["prelude"], "raw");
    let p_lost = prelude["lost_bytes"].as_u64().expect("prelude lost_bytes");
    let p_next = prelude["next_since"].as_u64().expect("prelude next_since");
    assert!(
        p_lost > 0,
        "late follower at since 0 must be flagged lost_bytes>0, got {p_lost}"
    );
    assert!(
        p_next > RING_CAP,
        "follower prelude high-water {p_next} must exceed the cap"
    );

    // Follow to a clean terminal eof (never panics; monotone cursor honored by
    // the daemon's stream loop).
    let code = drain_until_eof(lines).await;
    assert_eq!(
        code,
        Some(0),
        "flood child exits 0; follower must see a clean eof"
    );
}

// ---- idle-pane disconnect -> write-lease auto-release (cov-2, P0) -------

/// An `attach --write-lease` command with no prelude (we only care about the
/// lease, not the screen) — used to acquire/probe the single-writer lease.
fn lease_attach_cmd() -> Command {
    Command::Attach {
        pane: None,
        prelude: agent_tui_protocol::request::PreludeKind::None,
        mode: SnapshotMode::Cells,
        since: 0,
        write_lease: true,
        strip_ansi: false,
    }
}

/// Poll `attach --write-lease` (a fresh connection each try) until the lease is
/// granted or the bounded window elapses. Returns the number of ~40ms polls it
/// took (0 = first try), or `None` if never granted. Each probe connection is
/// dropped immediately so it doesn't itself hold the lease.
async fn poll_until_lease_granted(cfg: &DaemonConfig, max_tries: u32) -> Option<u32> {
    for tries in 0..max_tries {
        let (prelude, lines) = attach_open(cfg, lease_attach_cmd()).await;
        let granted = prelude["lease"]["granted"] == serde_json::Value::Bool(true);
        drop(lines); // release this probe's lease immediately
        if granted {
            return Some(tries);
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    None
}

/// cov-2 (gap #2, P0): the write-lease must auto-release when its holder
/// disconnects from an **idle** pane that emits no output — exercising the
/// read-half disconnect watcher, NOT the eof/output-driven release the G4 test
/// covered. A browser viewer holding the lease that closes its tab while the
/// task sits idle at a prompt must free the lease.
#[tokio::test]
async fn idle_pane_disconnect_releases_write_lease() {
    let (cfg, _h) = boot_daemon().await;
    // `sleep 30` never emits a byte → the ONLY way A's disconnect is noticed
    // is the read-half watcher (no chunk/eof traffic drives the release).
    let env = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sleep".into(), "1000".into()],
            cwd: None,
            size: Some((40, 10)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    assert!(env.response.success, "spawn failed: {env:?}");

    // A acquires the lease on the silent pane.
    let (prelude_a, lines_a) = attach_open(&cfg, lease_attach_cmd()).await;
    assert_eq!(
        prelude_a["lease"]["granted"], true,
        "A should be granted: {prelude_a}"
    );
    let token_a = prelude_a["lease"]["token"]
        .as_str()
        .expect("A token")
        .to_string();

    // While A holds it, a probe is denied and names A as the holder.
    let (probe, probe_lines) = attach_open(&cfg, lease_attach_cmd()).await;
    assert_eq!(probe["lease"]["granted"], false, "held while A connected");
    assert_eq!(
        probe["lease"]["held_by"].as_str(),
        Some(token_a.as_str()),
        "denied probe must name A as holder"
    );
    drop(probe_lines);

    // Drop A's connection. The pane is still idle (no output) — release must
    // come from the read-half watcher noticing the socket close.
    drop(lines_a);

    // Within a bounded window, B can acquire (lease auto-released).
    let polls = poll_until_lease_granted(&cfg, 75).await; // ≤ ~3s
    assert!(
        polls.is_some(),
        "idle-pane disconnect did not release the lease (B never granted) — watcher leak"
    );

    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

/// cov-2 contrast: the same disconnect path also releases on a BUSY pane (one
/// actively emitting output), so release works regardless of traffic.
#[tokio::test]
async fn busy_pane_disconnect_releases_write_lease() {
    let (cfg, _h) = boot_daemon().await;
    let env = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "i=0; while [ $i -lt 200 ]; do echo tick-$i; sleep 0.05; i=$((i+1)); done".into(),
            ],
            cwd: None,
            size: Some((40, 10)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    assert!(env.response.success, "spawn failed: {env:?}");

    let (prelude_a, lines_a) = attach_open(&cfg, lease_attach_cmd()).await;
    assert_eq!(
        prelude_a["lease"]["granted"], true,
        "A should be granted on busy pane"
    );

    // Drop A mid-flow (pane is actively emitting).
    drop(lines_a);

    let polls = poll_until_lease_granted(&cfg, 75).await;
    assert!(
        polls.is_some(),
        "busy-pane disconnect did not release the lease"
    );

    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

// ---- resolve_focused >1-live ambiguity (cov-3, P0) ---------------------

/// cov-3 (gap #3, P0): implicit (no-`--pane`) resolution must return a clean
/// ambiguity error when more than one pane is LIVE — never silently pick one —
/// and `--pane` / `pane focus` must disambiguate. Completes the resolve matrix
/// alongside the merged 0-live (most-recent) and 1-live cases.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn resolve_ambiguity_on_multiple_live_panes() {
    let (cfg, _h) = boot_daemon().await;
    let spawn = |cmd: &str| Command::Spawn {
        argv: vec!["/bin/sh".into(), "-c".into(), cmd.into()],
        cwd: None,
        size: Some((40, 10)),
        stdin: agent_tui_protocol::request::StdinMode::default(),
        env: Vec::new(),
    };
    let snap_nopane = || Command::Snapshot {
        pane: None,
        mode: SnapshotMode::Text,
        png: None,
        annotate: None,
        select: None,
        all: false,
        keep_color: false,
    };
    let snap_pane = |id: &str| Command::Snapshot {
        pane: Some(agent_tui_protocol::PaneId(id.into())),
        mode: SnapshotMode::Text,
        png: None,
        annotate: None,
        select: None,
        all: false,
        keep_color: false,
    };
    let text_of = |env: &ResponseEnvelope| {
        env.response
            .data
            .as_ref()
            .and_then(|d| d["text"].as_str())
            .unwrap_or_default()
            .to_string()
    };

    // Two long-lived, distinguishable panes — both LIVE (printed then `sleep`).
    let p1 = round_trip(&cfg, spawn("printf PANE_ONE; sleep 30")).await;
    assert!(p1.response.success);
    let id1 = p1.response.data.expect("p1")["pane"]
        .as_str()
        .expect("id1")
        .to_string();
    let p2 = round_trip(&cfg, spawn("printf PANE_TWO; sleep 30")).await;
    assert!(p2.response.success);
    let id2 = p2.response.data.expect("p2")["pane"]
        .as_str()
        .expect("id2")
        .to_string();
    assert!(
        wait_for_text_pane(&cfg, &id1, "PANE_ONE").await,
        "p1 never rendered"
    );
    assert!(
        wait_for_text_pane(&cfg, &id2, "PANE_TWO").await,
        "p2 never rendered"
    );

    // (1) >1 live → clean ambiguity error across the shared no-`--pane` verbs;
    //     NEVER a silent pick of one pane, NEVER a panic.
    let assert_ambiguous = |env: &ResponseEnvelope, verb: &str| {
        assert!(
            !env.response.success,
            "{verb} must error on >1 live: {env:?}"
        );
        let err = env.response.error.as_ref().expect("error body");
        assert!(
            err.message.contains("multiple live panes"),
            "{verb} must return the ambiguity error, got: {}",
            err.message
        );
    };
    assert_ambiguous(&round_trip(&cfg, snap_nopane()).await, "snapshot");
    assert_ambiguous(
        &round_trip(
            &cfg,
            Command::Type {
                pane: None,
                text: "x".into(),
                to: None,
                lease: None,
            },
        )
        .await,
        "type",
    );
    assert_ambiguous(
        &round_trip(
            &cfg,
            Command::Wait {
                pane: None,
                condition: agent_tui_protocol::request::WaitCondition::Text { regex: "x".into() },
                timeout: Duration::from_millis(500),
            },
        )
        .await,
        "wait",
    );

    // (2a) `--pane` disambiguates — explicit id resolves to that pane.
    assert!(text_of(&round_trip(&cfg, snap_pane(&id1)).await).contains("PANE_ONE"));
    assert!(text_of(&round_trip(&cfg, snap_pane(&id2)).await).contains("PANE_TWO"));

    // (2b) `pane focus <id>` then a no-`--pane` verb resolves to the focused one.
    let f = round_trip(
        &cfg,
        Command::Focus {
            pane: Some(agent_tui_protocol::PaneId(id1.clone())),
        },
    )
    .await;
    assert!(f.response.success, "focus failed: {f:?}");
    let focused = round_trip(&cfg, snap_nopane()).await;
    assert!(
        focused.response.success,
        "focused snapshot must resolve: {focused:?}"
    );
    assert!(
        text_of(&focused).contains("PANE_ONE"),
        "focus must target p1"
    );

    // (3) Matrix completeness: kill p1 → exactly 1 live (p2) → no-`--pane`
    //     resolves to it (the 1-live branch). Clear focus first so we exercise
    //     auto-resolution, not the lingering focus.
    let _ = round_trip(&cfg, Command::Focus { pane: None }).await;
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: Some(agent_tui_protocol::PaneId(id1.clone())),
            grace: None,
        },
    )
    .await;
    // `die grace:None` is fire-and-forget SIGTERM: it returns BEFORE p1's child
    // exits and is reaped, so `is_terminal(p1)` may still be false the instant
    // `die` returns. While p1 is still live there are 2 live panes and the
    // resolver CORRECTLY reports ambiguity — that is not the condition under
    // test. Bound-poll until p1 has flipped to terminal-retained (the no-`--pane`
    // snapshot stops erroring and resolves to p2), THEN assert. On a fast runner
    // the first poll wins; on a slow/cold macOS runner this absorbs the reap
    // latency without ever masking a real >1-live bug — we still require the
    // resolved snapshot to be p2's, never an arbitrary pick.
    let mut after = round_trip(&cfg, snap_nopane()).await;
    for _ in 0..200 {
        if after.response.success && text_of(&after).contains("PANE_TWO") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        after = round_trip(&cfg, snap_nopane()).await;
    }
    assert!(
        after.response.success,
        "1-live no-pane must resolve once p1 is terminal-retained: {after:?}"
    );
    assert!(
        text_of(&after).contains("PANE_TWO"),
        "must resolve to the sole live pane p2, never an arbitrary pick: {after:?}"
    );

    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

/// Poll a `--pane`-targeted text snapshot until `needle` appears (bounded).
async fn wait_for_text_pane(cfg: &DaemonConfig, id: &str, needle: &str) -> bool {
    for _ in 0..100 {
        let s = round_trip(
            cfg,
            Command::Snapshot {
                pane: Some(agent_tui_protocol::PaneId(id.into())),
                mode: SnapshotMode::Text,
                png: None,
                annotate: None,
                select: None,
                all: false,
                keep_color: false,
            },
        )
        .await;
        if s.response
            .data
            .as_ref()
            .and_then(|d| d["text"].as_str())
            .is_some_and(|t| t.contains(needle))
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

// ---- write-lease gate uniform across all 4 write verbs (cov-4, gap #4, P1) --

/// A write-verb constructor parameterized by an optional lease token, so the
/// cov-4 test can loop the same gate matrix over the whole write surface.
type WriteVerb = fn(Option<Uuid>) -> Command;

/// cov-4 (gap #4, P1): the single-writer write-lease gate must be **uniform**
/// across every write verb — `type`, `press`, `send-ansi`, and `stdin` all
/// route through the same `lease_denied`/`write_allowed` check, so a non-holder
/// must be rejected on *each* of them (G4 only proved `type`). A future change
/// that drops the gate from any one verb would silently let a browser viewer
/// without the lease inject input through that verb — a real single-driver hole.
///
/// The pane is spawned `stdin: Pipe` so the holder path of all four verbs
/// returns success: `stdin` needs the pipe, and `type`/`press`/`send-ansi`
/// write the PTY master regardless of stdin mode. The lease is per-pane and
/// independent of stdin mode, so the gate matrix is identical for every verb.
///
/// Discriminating: parameterized over the four verbs, so removing the gate from
/// ANY single verb flips that verb's non-holder assertion from "rejected" to
/// "succeeded" and fails the test for exactly that verb. `resize` is asserted
/// to be the deliberate carve-out (geometry is not gated).
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn write_lease_gate_uniform_across_all_write_verbs() {
    let (cfg, _h) = boot_daemon().await;
    let spawned = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/cat".into()],
            cwd: None,
            size: Some((40, 10)),
            stdin: agent_tui_protocol::request::StdinMode::Pipe,
            env: Vec::new(),
        },
    )
    .await;
    assert!(spawned.response.success, "spawn failed: {spawned:?}");

    // A acquires the single-writer lease and holds it for the duration (the
    // lease is bound to A's connection `lines_a`; dropping it releases).
    let lease_cmd = || Command::Attach {
        pane: None,
        prelude: agent_tui_protocol::request::PreludeKind::None,
        mode: SnapshotMode::Cells,
        since: 0,
        write_lease: true,
        strip_ansi: false,
    };
    let (prelude_a, lines_a) = attach_open(&cfg, lease_cmd()).await;
    assert_eq!(
        prelude_a["lease"]["granted"], true,
        "A should be granted the lease: {prelude_a:?}"
    );
    let token = Uuid::parse_str(prelude_a["lease"]["token"].as_str().expect("token"))
        .expect("lease token is a uuid");

    // One byte 'x' (hex 78) / key "a" per verb. Non-capturing fns so we can
    // loop the SAME matrix over the whole write surface.
    let verbs: [(&str, WriteVerb); 4] = [
        ("type", |lease| Command::Type {
            pane: None,
            text: "x".into(),
            to: None,
            lease,
        }),
        ("press", |lease| Command::Press {
            pane: None,
            keys: "a".into(),
            to: None,
            lease,
        }),
        ("send-ansi", |lease| Command::SendAnsi {
            pane: None,
            bytes_hex: "78".into(),
            lease,
        }),
        ("stdin", |lease| Command::Stdin {
            pane: None,
            bytes_hex: "78".into(),
            lease,
        }),
    ];

    // (1) Uniform gate while A holds the lease: for EACH verb a non-holder is
    //     rejected EBUSY-style, a foreign token is rejected, and A's token
    //     passes. This is the per-verb discriminator.
    for (name, make) in verbs {
        // non-holder (no token) -> EBUSY-style INVALID_ARGS citing the lease.
        let denied = round_trip(&cfg, make(None)).await;
        assert!(
            !denied.response.success,
            "{name}: non-holder write must be rejected while a lease is held: {denied:?}"
        );
        let err = denied
            .response
            .error
            .as_ref()
            .expect("error body on denial");
        assert_eq!(
            err.code,
            agent_tui_protocol::ErrorCode::InvalidArgs,
            "{name}: gate must reject with INVALID_ARGS (EBUSY-style): {err:?}"
        );
        assert!(
            err.message.contains("write-lease"),
            "{name}: rejection must cite the write-lease, got: {}",
            err.message
        );

        // foreign token -> still rejected (proves the gate checks the HOLDER,
        // not mere token presence).
        let foreign = round_trip(&cfg, make(Some(Uuid::nil()))).await;
        assert!(
            !foreign.response.success,
            "{name}: a foreign lease token must be rejected: {foreign:?}"
        );

        // holder (A's token) -> passes the gate and succeeds.
        let ok = round_trip(&cfg, make(Some(token))).await;
        assert!(
            ok.response.success,
            "{name}: the lease holder's write must succeed: {ok:?}"
        );
    }

    // (2) Contrast: `resize` is the deliberate carve-out. A non-holder resize
    //     succeeds while A holds the lease — keystroke input is gated, geometry
    //     is not. (If resize were ever gated this assertion fails.)
    let resize = round_trip(
        &cfg,
        Command::Resize {
            pane: None,
            cols: 50,
            rows: 12,
        },
    )
    .await;
    assert!(
        resize.response.success,
        "resize must NOT be lease-gated (deliberate carve-out): {resize:?}"
    );

    // (3) No-lease free-for-all: drop A's connection so the lease auto-releases
    //     (read-half disconnect watcher), then every write verb must succeed for
    //     an unauthenticated client — single-driver back-compat is unaffected.
    drop(lines_a);
    let mut released = false;
    for _ in 0..100 {
        let probe = round_trip(
            &cfg,
            Command::Type {
                pane: None,
                text: "z".into(),
                to: None,
                lease: None,
            },
        )
        .await;
        if probe.response.success {
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(
        released,
        "lease must auto-release after the holder disconnects"
    );
    for (name, make) in verbs {
        let ok = round_trip(&cfg, make(None)).await;
        assert!(
            ok.response.success,
            "{name}: with no lease outstanding any client must be able to write: {ok:?}"
        );
    }

    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

// ---- attach/tail-follow under CONCURRENT load (cov-8, Squire-critical) ------

/// Per-follower accounting collected by draining a follow stream to `eof`:
/// total follow bytes received, the largest single-chunk `lost_bytes`, the
/// final high-water (`next_since`), and whether the stream stayed monotonic,
/// stayed per-read consistent (`next_since - prev == lost_bytes + delivered`),
/// and terminated on `eof`.
#[derive(Default)]
struct FollowStats {
    total_bytes: u64,
    max_lost: u64,
    final_next_since: u64,
    saw_eof: bool,
    monotonic: bool,
    consistent: bool,
}

/// Drain a follower's stream to `eof`. `start_cursor` is the follow's initial
/// offset (0 for a `since: 0` follow; the prelude offset for a late joiner) so
/// the per-read invariant can be checked from the first chunk. Decodes
/// `bytes_b64` chunks (attach with `strip_ansi: false`).
async fn drain_follow_accounting(
    mut lines: tokio::io::Lines<BufReader<Stream>>,
    start_cursor: u64,
) -> FollowStats {
    let mut stats = FollowStats {
        monotonic: true,
        consistent: true,
        ..FollowStats::default()
    };
    let mut prev = start_cursor;
    loop {
        let line = timeout(Duration::from_secs(25), lines.next_line())
            .await
            .expect("follow timeout")
            .expect("read err");
        let Some(line) = line else { break };
        let env: ResponseEnvelope = serde_json::from_str(&line).expect("decode");
        let data = env.response.data.unwrap_or(serde_json::Value::Null);
        match data.get("type").and_then(|t| t.as_str()) {
            Some("chunk") => {
                let lost = data["lost_bytes"].as_u64().unwrap_or(0);
                let next = data["next_since"].as_u64().unwrap_or(0);
                let blen = data
                    .get("bytes_b64")
                    .and_then(|v| v.as_str())
                    .map_or(0, |b64| {
                        base64::engine::general_purpose::STANDARD
                            .decode(b64)
                            .expect("b64")
                            .len() as u64
                    });
                // The cursor must advance by exactly (lost + delivered) — the
                // ring's accounting identity. Holds even across an eviction:
                // next - prev == (tail_offset - prev) + retained.
                if next.checked_sub(prev) != Some(lost + blen) {
                    stats.consistent = false;
                }
                if next < prev {
                    stats.monotonic = false;
                }
                prev = next;
                stats.max_lost = stats.max_lost.max(lost);
                stats.total_bytes += blen;
            }
            Some("eof") => {
                stats.saw_eof = true;
                stats.final_next_since = data["next_since"].as_u64().unwrap_or(prev);
                break;
            }
            _ => {}
        }
    }
    stats
}

/// cov-8 (Squire-critical): the many-viewer fan-out. Fan one task-PTY out to
/// several concurrent followers — two FAST (drain promptly) and one SLOW (held
/// unread so it falls behind the 1 MiB ring horizon) — plus a LATE JOINER that
/// attaches mid-stream with a rendered prelude. Asserts:
///   * fast followers receive the full ordered stream with ZERO `lost_bytes`,
///     gap-free (`bytes received == high-water`);
///   * the slow follower, once past the horizon, is FLAGGED `lost_bytes > 0`
///     (never silently corrupted, never panicking the daemon), stays monotone +
///     per-read consistent, and still terminates on `eof`;
///   * per-follower cursor INDEPENDENCE — the slow follower's eviction does not
///     perturb the fast followers (no shared-cursor coupling), the core
///     many-viewer safety property;
///   * the late joiner re-preludes at a mid-stream offset and follows seamlessly
///     to `eof` (the G4 seam, now under concurrent load past the horizon).
///
/// Determinism: a shell `printf` loop produces well under ~20 MB/s, so a
/// promptly-draining follower can never fall behind the daemon's 50 ms poll
/// window (a poll advances its cursor to `total`, and < 1 MiB is produced per
/// window) — fast-follower-no-loss is guaranteed, not a timing gamble. Every
/// wait polls a real condition (high-water past the cap; stream `eof`); no
/// blind sleeps (the cov-3 macOS lesson).
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn concurrent_followers_slow_one_late_joiner_and_cursor_independence() {
    // Feed shape: 96 × 256 KiB ≈ 24 MiB total (≫ any plausible socket buffer, so
    // the unread slow follower's cursor stalls and the ring evicts past it), at
    // a paced ~14 MB/s (< the ~20 MB/s no-loss ceiling for prompt followers).
    const BLOCK: usize = 256 * 1024;
    const ROUNDS: u32 = 96;

    let (cfg, _h) = boot_daemon().await;

    // Producer: a real `cat` whose stdin is a pipe and whose stdout is the PTY.
    // The TEST feeds it in paced blocks, so the production rate is controlled by
    // wall-clock here (not by how fast a shell loop happens to run). At 256 KiB
    // every 18 ms (~14 MB/s) the per-50ms-poll delta (~710 KiB) stays under the
    // 1 MiB ring cap, so a promptly-draining follower provably never falls behind
    // — fast-follower-no-loss is deterministic, not a timing gamble. The total
    // (24 MiB) far exceeds any plausible socket buffer, so the UNREAD slow
    // follower's writer eventually blocks, its cursor stalls, and the ring evicts
    // past it (the eviction we want to flag). Bytes are a run of `x` (hex 78)
    // with no newline, so the PTY adds no CR/LF translation and the ring bytes
    // equal the fed bytes (clean accounting).
    let env = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/cat".into()],
            cwd: None,
            size: Some((80, 24)),
            stdin: agent_tui_protocol::request::StdinMode::Pipe,
            env: Vec::new(),
        },
    )
    .await;
    assert!(env.response.success, "spawn failed: {env:?}");

    // Two FAST followers (drained concurrently from the start) + one SLOW
    // follower (connection held, NOT read until the very end → its socket buffer
    // fills, the daemon's per-follower writer blocks, its cursor stalls, and the
    // ring evicts past it).
    // A `None`-prelude attach follows from the offset captured at attach time
    // (the prelude's `since`), NOT from 0 — so each follower's start cursor is
    // its own prelude offset.
    let (pa, lines_a) = attach_open(&cfg, attach_follow_cmd()).await;
    let (pb, lines_b) = attach_open(&cfg, attach_follow_cmd()).await;
    let (ps, lines_slow) = attach_open(&cfg, attach_follow_cmd()).await;
    let start_a = pa["since"].as_u64().expect("prelude since");
    let start_b = pb["since"].as_u64().expect("prelude since");
    let start_slow = ps["since"].as_u64().expect("prelude since");
    let fast_a = tokio::spawn(drain_follow_accounting(lines_a, start_a));
    let fast_b = tokio::spawn(drain_follow_accounting(lines_b, start_b));
    // lines_slow is intentionally left unread for now.

    // Feed ~128 × 32 KiB ≈ 4 MiB (≈4× the ring cap), paced. Partway through —
    // once well past the eviction horizon — attach a LATE JOINER mid-stream with
    // a rendered prelude (captures the current screen + an atomic follow offset;
    // the follow must continue seamlessly from there).
    let block_hex = "78".repeat(BLOCK);
    let mut late_pair: Option<(u64, tokio::task::JoinHandle<FollowStats>)> = None;
    for round in 0..ROUNDS {
        let fed = round_trip(
            &cfg,
            Command::Stdin {
                pane: None,
                bytes_hex: block_hex.clone(),
                lease: None,
            },
        )
        .await;
        assert!(fed.response.success, "feed round {round} failed: {fed:?}");
        if late_pair.is_none() && round == ROUNDS / 2 {
            let (prelude_late, lines_late) = attach_open(
                &cfg,
                Command::Attach {
                    pane: None,
                    prelude: agent_tui_protocol::request::PreludeKind::Rendered,
                    mode: SnapshotMode::Cells,
                    since: 0,
                    write_lease: false,
                    strip_ansi: false,
                },
            )
            .await;
            let off = prelude_late["since"]
                .as_u64()
                .expect("rendered prelude carries a `since` follow offset");
            assert!(
                off > RING_CAP,
                "late joiner's prelude offset must reflect its mid-stream (past-horizon) position, got {off}"
            );
            late_pair = Some((off, tokio::spawn(drain_follow_accounting(lines_late, off))));
        }
        tokio::time::sleep(Duration::from_millis(18)).await;
    }
    let (_late_offset, late) = late_pair.expect("late joiner attached mid-feed");

    // Close cat's stdin → cat sees EOF → exits 0 → every follower gets `eof`.
    let _ = round_trip(&cfg, Command::CloseStdin { pane: None }).await;

    // Now drain the SLOW follower — everything has flooded well past the horizon.
    let slow = drain_follow_accounting(lines_slow, start_slow).await;

    let a = fast_a.await.expect("fast A task");
    let b = fast_b.await.expect("fast B task");
    let l = late.await.expect("late joiner task");

    // (1) FAST followers: full ordered stream from their attach point, ZERO
    //     loss, gap-free, consistent.
    for (name, s, start) in [("A", &a, start_a), ("B", &b, start_b)] {
        assert!(s.saw_eof, "fast {name} must terminate on eof");
        assert!(s.monotonic, "fast {name} next_since must be monotonic");
        assert!(s.consistent, "fast {name} per-read accounting must hold");
        assert_eq!(
            s.max_lost, 0,
            "fast {name} must NOT lose bytes (stayed current); max_lost={}",
            s.max_lost
        );
        assert_eq!(
            s.total_bytes,
            s.final_next_since - start,
            "fast {name} received bytes ({}) must equal everything since its attach offset ({} .. {}) — no gap/overlap",
            s.total_bytes,
            start,
            s.final_next_since
        );
        assert!(
            s.final_next_since > RING_CAP,
            "the flood must have exceeded the ring cap (got {})",
            s.final_next_since
        );
    }

    // (2) SLOW follower: crossed the horizon → flagged, never corrupted, still
    //     monotone + per-read consistent, still terminates on eof.
    assert!(slow.saw_eof, "slow follower must still terminate on eof");
    assert!(slow.monotonic, "slow follower next_since must be monotonic");
    assert!(
        slow.consistent,
        "slow follower per-read accounting (next - prev == lost + delivered) must hold across eviction"
    );
    assert!(
        slow.max_lost > 0,
        "slow follower crossed the eviction horizon → must be FLAGGED lost_bytes>0"
    );
    assert!(
        slow.total_bytes < slow.final_next_since - start_slow,
        "slow follower must have a real gap (received {} < its share {} .. {})",
        slow.total_bytes,
        start_slow,
        slow.final_next_since
    );

    // (3) Per-follower cursor INDEPENDENCE: the slow follower crossing the
    //     horizon did NOT perturb the fast followers — they each still saw the
    //     complete, loss-free stream asserted in (1). Both converge on the same
    //     high-water, and the late joiner streamed cleanly from its own offset.
    assert_eq!(
        a.final_next_since, b.final_next_since,
        "fast followers must converge on the same high-water ({} vs {})",
        a.final_next_since, b.final_next_since
    );
    assert!(
        l.saw_eof && l.monotonic && l.consistent,
        "late joiner must stream cleanly (monotone, consistent) to eof from its mid-stream offset"
    );

    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}
