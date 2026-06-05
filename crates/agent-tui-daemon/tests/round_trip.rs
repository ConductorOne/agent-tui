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
            grace: None,
        },
    )
    .await;

    let list2 = round_trip(&cfg, Command::List { all: false }).await;
    let panes_after = list2.response.data.expect("list data");
    assert_eq!(panes_after["panes"].as_array().expect("array").len(), 0);
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
    // cell metrics are 8×8, so a 40×4 grid → 320×32 px.
    assert_eq!(data["png"]["width"], 40 * 8, "png width = cols*cw");
    assert_eq!(data["png"]["height"], 4 * 8, "png height = rows*ch");
    assert_eq!(data["png"]["annotated"], false, "no overlay requested");

    // Decode it from disk: a valid image of exactly the reported size.
    let (w, h, pixels) = decode_png(&path);
    assert_eq!((w, h), (320, 32), "decoded PNG dims");
    assert_eq!(
        pixels.len(),
        (w * h * 3) as usize,
        "RGB buffer is fully populated"
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
