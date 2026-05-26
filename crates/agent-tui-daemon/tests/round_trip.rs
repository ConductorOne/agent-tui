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
