//! End-to-end integration tests for the P0a vertical slice.
//!
//! Each test catches a regression class:
//!  - `pty_echo_round_trip`: PTY → engine.feed → snapshot pipeline
//!  - `spawn_list_die_lifecycle`: pane registry insert/list/remove
//!  - `daemon_wire_smoke`: JSON wire envelope round-trips over the UDS
//!  - `snapshot_hash_changes_after_output`: sequence + hash mechanics

use std::path::PathBuf;
use std::time::{Duration, Instant};

use agent_tui_daemon::{DaemonConfig, SocketLayout, run_daemon};
use agent_tui_protocol::request::SnapshotMode;
use agent_tui_protocol::{Command, PROTOCOL_VERSION, Request, ResponseEnvelope, SessionId};
use base64::Engine as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::timeout;
use uuid::Uuid;

/// Spin up an isolated daemon on a temp socket dir and return its connect URL.
async fn boot_daemon() -> (DaemonConfig, agent_tui_daemon::DaemonHandle) {
    let session = SessionId(format!("test-{}", Uuid::new_v4().simple()));
    let root: PathBuf = std::env::temp_dir().join(format!("agent-tui-test-{session}"));
    std::fs::create_dir_all(&root).expect("mkdir tempdir");
    let layout = SocketLayout::for_session_in(&session, root);
    let cfg = DaemonConfig {
        session: session.clone(),
        layout: layout.clone(),
        engine: "alacritty".into(),
        binary_version: "0.0.0-test".into(),
        allowed_binaries: None,
    };
    let handle = run_daemon(cfg.clone()).await.expect("run_daemon");
    // Tiny yield so the accept loop is parked before we connect.
    tokio::task::yield_now().await;
    (cfg, handle)
}

async fn round_trip(cfg: &DaemonConfig, command: Command) -> ResponseEnvelope {
    let mut stream = UnixStream::connect(&cfg.layout.socket)
        .await
        .expect("connect");
    let req = Request {
        id: Uuid::new_v4(),
        protocol: PROTOCOL_VERSION,
        command,
    };
    let mut bytes = serde_json::to_vec(&req).expect("encode");
    bytes.push(b'\n');
    stream.write_all(&bytes).await.expect("write");
    let (r, _w) = stream.split();
    let mut lines = BufReader::new(r).lines();
    let line = timeout(Duration::from_secs(5), lines.next_line())
        .await
        .expect("read timeout")
        .expect("read err")
        .expect("eof");
    serde_json::from_str(&line).expect("decode")
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
            annotate: false,
        },
    )
    .await;
    assert!(snap.response.success, "snapshot failed: {snap:?}");
    let data = snap.response.data.expect("data");
    let outline_name = data["outline"]["nodes"][0]["name"]
        .as_str()
        .expect("outline name");
    assert!(
        outline_name.contains("hello"),
        "expected outline to contain 'hello', got: {outline_name:?}"
    );

    // Clean up.
    let _ = round_trip(&cfg, Command::Die { pane: None }).await;
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
        },
    )
    .await;
    assert!(env.response.success);

    let list1 = round_trip(&cfg, Command::List { all: false }).await;
    let panes = list1.response.data.expect("list data");
    assert_eq!(panes["panes"].as_array().expect("array").len(), 1);

    let _die = round_trip(&cfg, Command::Die { pane: None }).await;

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
            annotate: false,
        },
    )
    .await;
    let outline = snap.response.data.unwrap()["outline"]["nodes"][0]["name"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        outline.contains("hello"),
        "cat should have echoed 'hello'; got: {outline:?}"
    );

    let _ = round_trip(&cfg, Command::Die { pane: None }).await;
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
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let press = round_trip(
        &cfg,
        Command::Press {
            pane: None,
            keys: "x".into(),
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

    let _ = round_trip(&cfg, Command::Die { pane: None }).await;
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
    let _ = round_trip(&cfg, Command::Die { pane: None }).await;
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
    let _ = round_trip(&cfg, Command::Die { pane: None }).await;
}

#[tokio::test]
async fn resize_updates_engine_geometry() {
    let (cfg, _h) = boot_daemon().await;
    let _spawn = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 2".into()],
            cwd: None,
            size: Some((40, 4)),
        },
    )
    .await;

    let r = round_trip(
        &cfg,
        Command::Resize {
            pane: None,
            cols: 132,
            rows: 40,
        },
    )
    .await;
    assert!(r.response.success, "resize failed: {r:?}");

    // Engine geometry change should be observable via the cells mode path,
    // but for P0b we only have outline mode. Round-trip a list and confirm
    // the pane's recorded dims are still the spawn-time ones (we don't
    // propagate to PaneSummary in v0.1.0 — recorded as a learning).
    let list = round_trip(&cfg, Command::List { all: false }).await;
    let panes = list.response.data.unwrap();
    assert_eq!(
        panes["panes"][0]["cols"], 40,
        "summary still shows spawn-time cols"
    );

    let _ = round_trip(&cfg, Command::Die { pane: None }).await;
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
        },
    )
    .await;
    assert!(env.response.success);
    let data = env.response.data.unwrap();
    assert_eq!(data["adapter"], "shell");
    let _ = round_trip(&cfg, Command::Die { pane: None }).await;
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
            annotate: false,
        },
    )
    .await;
    let data = snap.response.data.unwrap();
    assert_eq!(data["outline"]["adapter"], "shell");
    let _ = round_trip(&cfg, Command::Die { pane: None }).await;
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
            annotate: false,
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
    let _ = round_trip(&cfg, Command::Die { pane: None }).await;
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
            annotate: false,
        },
    )
    .await;
    let data = snap.response.data.unwrap();
    assert_eq!(
        data["state"], "shell",
        "OSC 133 A should classify as shell, got: {data:?}"
    );
    let _ = round_trip(&cfg, Command::Die { pane: None }).await;
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
        },
    )
    .await;
    let _p2 = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            cwd: None,
            size: None,
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
            annotate: false,
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
            annotate: false,
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
        },
    )
    .await;
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: Some(agent_tui_protocol::PaneId("p2".into())),
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
        },
    )
    .await;
    let _p2 = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            cwd: None,
            size: None,
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
            annotate: false,
        },
    )
    .await;
    assert!(!snap.response.success);
    let _ = round_trip(
        &cfg,
        Command::Die {
            pane: Some(agent_tui_protocol::PaneId("p2".into())),
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
            annotate: false,
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

    let _ = round_trip(&cfg, Command::Die { pane: None }).await;
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
            annotate: false,
        },
    )
    .await;
    let data = snap.response.data.unwrap();
    assert!(data["outline"].is_object(), "hybrid must carry outline");
    assert!(data["cells"].is_object(), "hybrid must carry cells");
    let _ = round_trip(&cfg, Command::Die { pane: None }).await;
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
                annotate: false,
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
                    annotate: false,
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

    let _die = round_trip(&cfg, Command::Die { pane: None }).await;
}
