//! End-to-end tests for the `events` streaming verb and the screen-model
//! fields it shares with `snapshot` (geometry, cursor, title, output age).
//!
//! Each test catches a regression class:
//!  - `events_init_then_screen_changed_then_child_exited`: the stream's
//!    lifecycle contract (baseline → change → terminal event, then EOF)
//!  - `events_throttles_screen_changed_bursts`: the debounce window
//!    coalesces a redraw burst instead of emitting per-mutation
//!  - `events_reports_bell_and_mode_change`: BEL and alt-screen flips
//!    surface as their own event types
//!  - `snapshot_carries_geometry_cursor_and_output_age`: the new
//!    top-level snapshot fields are present in every mode
//!
//! Gated `cfg(unix)` like `round_trip.rs` — every test spawns POSIX shells.

#![cfg(unix)]

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

/// Spin up an isolated daemon on a temp socket dir (mirrors `round_trip.rs`).
async fn boot_daemon() -> (DaemonConfig, agent_tui_daemon::DaemonHandle) {
    let session = SessionId(short_session());
    let root = short_temp_root("at-ev");
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
        adopt_handoff: None,
    };
    let handle = run_daemon(cfg.clone()).await.expect("run_daemon");
    tokio::task::yield_now().await;
    (cfg, handle)
}

fn short_session() -> String {
    let mut h = Uuid::new_v4().simple().to_string();
    h.truncate(8);
    h
}

fn short_temp_root(prefix: &str) -> PathBuf {
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

fn spawn_cmd(script: &str) -> Command {
    Command::Spawn {
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        cwd: None,
        size: None,
        stdin: agent_tui_protocol::request::StdinMode::Pty,
        env: Vec::new(),
    }
}

/// Open an `events` stream and return the `init` event plus the line reader.
async fn events_open(
    cfg: &DaemonConfig,
    debounce_ms: Option<u64>,
) -> (serde_json::Value, tokio::io::Lines<BufReader<Stream>>) {
    let name = agent_tui_daemon::paths::socket_name(&cfg.layout).expect("name");
    let mut stream = Stream::connect(name).await.expect("connect");
    let req = Request {
        id: Uuid::new_v4(),
        protocol: PROTOCOL_VERSION,
        command: Command::Events {
            pane: None,
            debounce_ms,
        },
    };
    let mut bytes = serde_json::to_vec(&req).expect("encode");
    bytes.push(b'\n');
    stream.write_all(&bytes).await.expect("write");
    let mut lines = BufReader::new(stream).lines();
    let line = timeout(Duration::from_secs(10), lines.next_line())
        .await
        .expect("init timeout")
        .expect("read err")
        .expect("eof before init");
    let env: ResponseEnvelope = serde_json::from_str(&line).expect("decode");
    assert!(env.response.success, "events open failed: {env:?}");
    let data = env.response.data.expect("init data");
    assert_eq!(
        data.get("type").and_then(|t| t.as_str()),
        Some("init"),
        "first event must be init: {data}"
    );
    (data, lines)
}

/// Drain events until `child_exited` (or the deadline), returning every
/// event payload in order, including the terminal one.
async fn drain_events(
    mut lines: tokio::io::Lines<BufReader<Stream>>,
    deadline: Duration,
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let drain = async {
        loop {
            let Some(line) = lines.next_line().await.expect("read err") else {
                break;
            };
            let env: ResponseEnvelope = serde_json::from_str(&line).expect("decode");
            let data = env.response.data.unwrap_or(serde_json::Value::Null);
            let ty = data.get("type").and_then(|t| t.as_str()).map(String::from);
            out.push(data);
            if ty.as_deref() == Some("child_exited") {
                break;
            }
        }
    };
    let _ = timeout(deadline, drain).await;
    out
}

fn types_of(events: &[serde_json::Value]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| e.get("type").and_then(|t| t.as_str()).map(String::from))
        .collect()
}

#[tokio::test]
async fn events_init_then_screen_changed_then_child_exited() {
    let (cfg, _h) = boot_daemon().await;
    // Child waits, prints, exits 7 — so the init baseline lands before any
    // output, then exactly one burst, then a clean exit.
    let env = round_trip(
        &cfg,
        spawn_cmd("sleep 0.3 && printf 'marker-text' && sleep 0.2 && exit 7"),
    )
    .await;
    assert!(env.response.success, "spawn failed: {env:?}");

    let (init, lines) = events_open(&cfg, Some(50)).await;
    let init_hash = init
        .get("screen_hash")
        .and_then(|h| h.as_str())
        .expect("init carries screen_hash")
        .to_string();
    assert_eq!(init_hash.len(), 64, "hex SHA-256");
    assert!(init.get("cols").and_then(serde_json::Value::as_u64) > Some(0));
    assert!(init.get("rows").and_then(serde_json::Value::as_u64) > Some(0));
    assert!(
        init.get("cursor").and_then(|c| c.get("visible")).is_some(),
        "init carries cursor visibility: {init}"
    );

    let events = drain_events(lines, Duration::from_secs(15)).await;
    let types = types_of(&events);
    assert!(
        types.contains(&"screen_changed".to_string()),
        "output must produce screen_changed: {types:?}"
    );
    let changed = events
        .iter()
        .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("screen_changed"))
        .expect("screen_changed present");
    let new_hash = changed
        .get("screen_hash")
        .and_then(|h| h.as_str())
        .expect("screen_changed carries screen_hash");
    assert_ne!(new_hash, init_hash, "hash advances when the screen changes");

    let last = events.last().expect("at least one event");
    assert_eq!(
        last.get("type").and_then(|t| t.as_str()),
        Some("child_exited"),
        "stream ends with child_exited: {types:?}"
    );
    assert_eq!(
        last.get("exit_code").and_then(serde_json::Value::as_i64),
        Some(7),
        "child_exited carries the remembered exit code"
    );
}

#[tokio::test]
async fn events_throttles_screen_changed_bursts() {
    let (cfg, _h) = boot_daemon().await;
    // 40 separate writes, ~5ms apart = a ~200ms redraw burst. With a 500ms
    // throttle window every line lands inside one or two flushes — far
    // fewer screen_changed events than mutations.
    let env = round_trip(
        &cfg,
        spawn_cmd(
            "sleep 0.3; i=0; while [ $i -lt 40 ]; do echo line-$i; sleep 0.005; i=$((i+1)); done",
        ),
    )
    .await;
    assert!(env.response.success, "spawn failed: {env:?}");

    let (_init, lines) = events_open(&cfg, Some(500)).await;
    let events = drain_events(lines, Duration::from_secs(20)).await;
    let changed = types_of(&events)
        .iter()
        .filter(|t| *t == "screen_changed")
        .count();
    assert!(
        changed >= 1,
        "burst must surface at least one screen_changed"
    );
    assert!(
        changed <= 5,
        "40 writes under a 500ms throttle must coalesce hard, got {changed} \
         screen_changed events: {:?}",
        types_of(&events)
    );
}

#[tokio::test]
async fn events_reports_bell_and_mode_change() {
    let (cfg, _h) = boot_daemon().await;
    let env = round_trip(
        &cfg,
        // Enter alt-screen, ring the bell, leave, exit — with settling gaps.
        spawn_cmd(
            "sleep 0.3; printf '\\033[?1049h'; sleep 0.2; printf '\\a'; \
             sleep 0.2; printf '\\033[?1049l'; sleep 0.2",
        ),
    )
    .await;
    assert!(env.response.success, "spawn failed: {env:?}");

    let (init, lines) = events_open(&cfg, Some(50)).await;
    assert_eq!(
        init.get("alt_screen").and_then(serde_json::Value::as_bool),
        Some(false),
        "baseline starts on the primary screen"
    );
    let events = drain_events(lines, Duration::from_secs(15)).await;
    let types = types_of(&events);
    assert!(
        types.contains(&"bell".to_string()),
        "BEL must surface as a bell event: {types:?}"
    );
    let mode_flips: Vec<bool> = events
        .iter()
        .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("mode_changed"))
        .filter_map(|e| e.get("alt_screen").and_then(serde_json::Value::as_bool))
        .collect();
    assert_eq!(
        mode_flips,
        vec![true, false],
        "alt-screen enter then exit must surface as two mode_changed events: {types:?}"
    );
}

#[tokio::test]
async fn snapshot_carries_geometry_cursor_and_output_age() {
    let (cfg, _h) = boot_daemon().await;
    let env = round_trip(&cfg, spawn_cmd("printf 'hello' && sleep 5")).await;
    assert!(env.response.success, "spawn failed: {env:?}");

    // Give the child a beat to produce output.
    tokio::time::sleep(Duration::from_millis(400)).await;

    let env = round_trip(
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
    assert!(env.response.success, "snapshot failed: {env:?}");
    let data = env.response.data.expect("snapshot data");

    // Geometry at the top level in EVERY mode (not just cells).
    assert_eq!(
        data.get("cols").and_then(serde_json::Value::as_u64),
        Some(80)
    );
    assert_eq!(
        data.get("rows").and_then(serde_json::Value::as_u64),
        Some(24)
    );

    // Cursor object with visibility.
    let cursor = data.get("cursor").expect("cursor present");
    assert!(cursor.get("row").is_some() && cursor.get("col").is_some());
    assert_eq!(
        cursor.get("visible").and_then(serde_json::Value::as_bool),
        Some(true),
        "shell cursor is visible"
    );

    // The child wrote "hello" ≥0ms ago and the field is a sane age.
    let age = data
        .get("last_output_ms_ago")
        .and_then(serde_json::Value::as_u64)
        .expect("last_output_ms_ago present after output");
    assert!(age < 60_000, "age is recent, got {age}ms");

    // Existing fields unchanged alongside the new ones.
    assert!(data.get("hash").is_some() && data.get("text").is_some());
}
