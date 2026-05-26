//! Windows smoke test for the daemon round-trip.
//!
//! Spawns `cmd.exe /c echo hi & ping -n 2 127.0.0.1 > nul` — a one-liner
//! that prints something then idles ~1 second so we can snapshot before
//! the child exits.

#![cfg(windows)]

use std::path::PathBuf;
use std::time::Duration;

use agent_tui_daemon::{DaemonConfig, SocketLayout, run_daemon};
use agent_tui_protocol::request::SnapshotMode;
use agent_tui_protocol::{Command, PROTOCOL_VERSION, Request, ResponseEnvelope, SessionId};
use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::traits::tokio::Stream as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;
use uuid::Uuid;

async fn boot_daemon() -> (DaemonConfig, agent_tui_daemon::DaemonHandle) {
    let mut sid = Uuid::new_v4().simple().to_string();
    sid.truncate(8);
    let session = SessionId(sid);
    // Windows: socket name is namespaced via session basename, so root
    // doesn't materially affect addressability. Use the temp dir directly.
    let root: PathBuf = std::env::temp_dir().join("at-ws");
    std::fs::create_dir_all(&root).expect("mkdir tempdir");
    let layout = SocketLayout::for_session_in(&session, root);
    let cfg = DaemonConfig {
        session,
        layout,
        engine: "alacritty".into(),
        binary_version: "0.0.0-test".into(),
        allowed_binaries: None,
    };
    let handle = run_daemon(cfg.clone()).await.expect("run_daemon");
    tokio::task::yield_now().await;
    (cfg, handle)
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
    let line = timeout(Duration::from_secs(15), lines.next_line())
        .await
        .expect("read timeout")
        .expect("read err")
        .expect("eof");
    serde_json::from_str(&line).expect("decode")
}

#[tokio::test]
async fn windows_spawn_cmd_echoes() {
    let (cfg, _h) = boot_daemon().await;
    let env = round_trip(
        &cfg,
        Command::Spawn {
            argv: vec![
                "cmd.exe".into(),
                "/c".into(),
                "echo hi & ping -n 2 127.0.0.1 > nul".into(),
            ],
            cwd: None,
            size: Some((40, 4)),
        },
    )
    .await;
    assert!(env.response.success, "spawn cmd.exe: {env:?}");

    tokio::time::sleep(Duration::from_millis(300)).await;

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
    assert!(snap.response.success, "snapshot: {snap:?}");

    let _ = round_trip(&cfg, Command::Die { pane: None }).await;
}

#[tokio::test]
async fn windows_daemon_wire_smoke() {
    let (cfg, _h) = boot_daemon().await;
    let env = round_trip(&cfg, Command::DaemonStatus).await;
    assert!(env.response.success, "DaemonStatus must succeed");
    assert_eq!(env.protocol, PROTOCOL_VERSION);
}
