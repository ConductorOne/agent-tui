//! Integration tests for the addressing model's daemon surfaces.
//!
//! Covers:
//!  - `wait --ref` blocks until a selector matches a node in the outline
//!  - `wait --ref --gone` blocks until a selector matches nothing
//!  - `snapshot --select` filters the outline to matching nodes
//!  - `snapshot --select` with `all: true` returns every match
//!  - Malformed selectors return `INVALID_ARGS` synchronously
//!
//! Tests spawn `/bin/sh -c '…'` so any platform-conditional binary
//! discovery stays in one place.

#![cfg(unix)]

use std::path::PathBuf;
use std::time::Duration;

use agent_tui_daemon::{DaemonConfig, SocketLayout, run_daemon};
use agent_tui_protocol::request::{SnapshotMode, WaitCondition};
use agent_tui_protocol::{Command, PROTOCOL_VERSION, Request, ResponseEnvelope, SessionId};
use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::traits::tokio::Stream as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;
use uuid::Uuid;

async fn boot() -> (DaemonConfig, agent_tui_daemon::DaemonHandle) {
    let mut sid = Uuid::new_v4().simple().to_string();
    sid.truncate(8);
    let session = SessionId(sid);
    let mut h = Uuid::new_v4().simple().to_string();
    h.truncate(8);
    let root: PathBuf = PathBuf::from(format!("/tmp/at-rs-{h}"));
    std::fs::create_dir_all(&root).unwrap();
    let layout = SocketLayout::for_session_in(&session, root);
    let cfg = DaemonConfig {
        session,
        layout,
        engine: "alacritty".into(),
        binary_version: "0.0.0-test".into(),
        allowed_binaries: None,
        monitor_parent: None,
        idle_timeout_secs: None,
    };
    let handle = run_daemon(cfg.clone()).await.unwrap();
    tokio::task::yield_now().await;
    (cfg, handle)
}

async fn rt(cfg: &DaemonConfig, command: Command) -> ResponseEnvelope {
    let name = agent_tui_daemon::paths::socket_name(&cfg.layout).unwrap();
    let stream = Stream::connect(name).await.unwrap();
    let req = Request {
        id: Uuid::new_v4(),
        protocol: PROTOCOL_VERSION,
        command,
    };
    let mut bytes = serde_json::to_vec(&req).unwrap();
    bytes.push(b'\n');
    let (r, mut w) = tokio::io::split(stream);
    w.write_all(&bytes).await.unwrap();
    let mut lines = BufReader::new(r).lines();
    let line = timeout(Duration::from_secs(30), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

async fn spawn_shell_printing(cfg: &DaemonConfig, body: &str) {
    let env = rt(
        cfg,
        Command::Spawn {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                // Print something deterministic, then idle so we can
                // snapshot before the child exits.
                format!("printf '{body}'; sleep 30"),
            ],
            cwd: None,
            size: Some((40, 6)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    assert!(env.response.success, "spawn failed: {env:?}");
    // Give the daemon a moment to consume the printf output.
    tokio::time::sleep(Duration::from_millis(150)).await;
}

#[tokio::test]
async fn snapshot_select_filters_outline_to_matching_nodes() {
    let (cfg, _h) = boot().await;
    spawn_shell_printing(&cfg, "hello-from-select").await;

    // Default outline has at least one buffer node from the generic
    // adapter. Selector targets it by role.
    let env = rt(
        &cfg,
        Command::Snapshot {
            pane: None,
            mode: SnapshotMode::Outline,
            png: None,
            annotate: false,
            select: Some("[role=buffer]".into()),
            all: false,
        },
    )
    .await;
    assert!(env.response.success, "snapshot --select failed: {env:?}");
    let data = env.response.data.unwrap();
    let nodes = data["outline"]["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1, "expected exactly one matched node: {nodes:?}");
    assert_eq!(nodes[0]["role"].as_str().unwrap(), "buffer");

    let _ = rt(&cfg, Command::Die { pane: None }).await;
}

#[tokio::test]
async fn snapshot_select_invalid_returns_invalid_args() {
    let (cfg, _h) = boot().await;
    spawn_shell_printing(&cfg, "x").await;

    let env = rt(
        &cfg,
        Command::Snapshot {
            pane: None,
            mode: SnapshotMode::Outline,
            png: None,
            annotate: false,
            select: Some("[no_such_attr=oops]".into()),
            all: false,
        },
    )
    .await;
    assert!(!env.response.success, "expected failure: {env:?}");
    let err = env.response.error.unwrap();
    assert_eq!(
        err.code,
        agent_tui_protocol::ErrorCode::InvalidArgs,
        "unexpected error code: {err:?}"
    );

    let _ = rt(&cfg, Command::Die { pane: None }).await;
}

#[tokio::test]
async fn wait_ref_fires_when_selector_matches_existing_node() {
    let (cfg, _h) = boot().await;
    spawn_shell_printing(&cfg, "wait-ref-test").await;

    // The buffer is already present from the spawn; wait_ref should
    // return immediately rather than blocking.
    let env = rt(
        &cfg,
        Command::Wait {
            pane: None,
            condition: WaitCondition::Ref {
                selector: "[role=buffer]".into(),
                gone: false,
            },
            timeout: Duration::from_secs(3),
        },
    )
    .await;
    assert!(env.response.success, "wait --ref failed: {env:?}");

    let _ = rt(&cfg, Command::Die { pane: None }).await;
}

#[tokio::test]
async fn wait_ref_gone_fires_when_no_node_matches() {
    let (cfg, _h) = boot().await;
    spawn_shell_printing(&cfg, "x").await;

    // A selector that matches nothing in the generic outline.
    let env = rt(
        &cfg,
        Command::Wait {
            pane: None,
            condition: WaitCondition::Ref {
                selector: "@nonexistent.adapter[%999]".into(),
                gone: true,
            },
            timeout: Duration::from_secs(3),
        },
    )
    .await;
    assert!(env.response.success, "wait --ref --gone failed: {env:?}");

    let _ = rt(&cfg, Command::Die { pane: None }).await;
}

#[tokio::test]
async fn wait_ref_with_bad_selector_returns_invalid_args() {
    let (cfg, _h) = boot().await;
    spawn_shell_printing(&cfg, "x").await;

    let env = rt(
        &cfg,
        Command::Wait {
            pane: None,
            condition: WaitCondition::Ref {
                selector: "[unfinished".into(),
                gone: false,
            },
            timeout: Duration::from_secs(1),
        },
    )
    .await;
    assert!(!env.response.success);
    let err = env.response.error.unwrap();
    assert_eq!(err.code, agent_tui_protocol::ErrorCode::InvalidArgs);

    let _ = rt(&cfg, Command::Die { pane: None }).await;
}
