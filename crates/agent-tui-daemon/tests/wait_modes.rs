//! Integration tests for every `wait` mode.
//!
//! Catches regression classes:
//!  - `wait_since_returns_on_next_mutation`: subscription + dispatch loop
//!  - `wait_idle_returns_after_quiet_period`: quiet-period timer reset
//!  - `wait_text_matches_when_pattern_appears`: regex over visible buffer
//!  - `wait_alt_screen_returns_on_toggle`: mode-flag matching
//!  - `wait_hash_unknown_returns_error`: seq->hash window miss path
//!  - `wait_timeout_returns_wait_timeout`: deadline path on a noisy pane
//!
//! Gated `cfg(unix)`: every test spawns POSIX `/bin/cat` or `/bin/sh`.

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

async fn boot_daemon() -> (DaemonConfig, agent_tui_daemon::DaemonHandle) {
    // macOS sun_path is 104 bytes; keep the socket path tiny.
    let mut sid = Uuid::new_v4().simple().to_string();
    sid.truncate(8);
    let session = SessionId(sid);
    let mut h = Uuid::new_v4().simple().to_string();
    h.truncate(8);
    let root: PathBuf = PathBuf::from(format!("/tmp/at-wm-{h}"));
    std::fs::create_dir_all(&root).expect("mkdir tempdir");
    let layout = SocketLayout::for_session_in(&session, root);
    let cfg = DaemonConfig {
        session,
        layout,
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

async fn rt(cfg: &DaemonConfig, command: Command) -> ResponseEnvelope {
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
    let line = timeout(Duration::from_secs(30), lines.next_line())
        .await
        .expect("read timeout")
        .expect("read err")
        .expect("eof");
    serde_json::from_str(&line).expect("decode")
}

async fn spawn_cat(cfg: &DaemonConfig) {
    let env = rt(
        cfg,
        Command::Spawn {
            argv: vec!["/bin/cat".into()],
            cwd: None,
            size: Some((40, 4)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    assert!(env.response.success, "spawn cat: {env:?}");
    tokio::time::sleep(Duration::from_millis(50)).await;
}

async fn snap_sequence(cfg: &DaemonConfig) -> u64 {
    let env = rt(
        cfg,
        Command::Snapshot {
            pane: None,
            mode: SnapshotMode::Outline,
            png: None,
            annotate: None,
            chrome: None,
            select: None,
            all: false,
            keep_color: false,
        },
    )
    .await;
    env.response.data.unwrap()["sequence"].as_u64().unwrap()
}

#[tokio::test]
async fn wait_since_returns_on_next_mutation() {
    let (cfg, _h) = boot_daemon().await;
    spawn_cat(&cfg).await;
    let baseline = snap_sequence(&cfg).await;

    // Kick a wait off in the background, then press to unblock it.
    let cfg_w = cfg.clone();
    let waiting = tokio::spawn(async move {
        rt(
            &cfg_w,
            Command::Wait {
                pane: None,
                condition: WaitCondition::Since { since: baseline },
                timeout: Duration::from_secs(5),
            },
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let _press = rt(
        &cfg,
        Command::Press {
            pane: None,
            keys: "x".into(),
            to: None,
            lease: None,
        },
    )
    .await;

    let waited = waiting.await.expect("waiter joined");
    assert!(waited.response.success, "wait failed: {waited:?}");
    let seq = waited.response.data.unwrap()["sequence"].as_u64().unwrap();
    assert!(
        seq > baseline,
        "post-wait seq {seq} must exceed baseline {baseline}"
    );

    let _ = rt(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn wait_idle_returns_after_quiet_period() {
    let (cfg, _h) = boot_daemon().await;
    spawn_cat(&cfg).await;
    let env = rt(
        &cfg,
        Command::Wait {
            pane: None,
            condition: WaitCondition::Idle { quiet_ms: 80 },
            timeout: Duration::from_secs(3),
        },
    )
    .await;
    assert!(
        env.response.success,
        "idle wait should succeed on a quiet pane: {env:?}"
    );
    let _ = rt(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn wait_text_matches_when_pattern_appears() {
    let (cfg, _h) = boot_daemon().await;
    let _spawn = rt(
        &cfg,
        Command::Spawn {
            argv: vec!["/bin/sh".into(), "-c".into(), "printf hello-world".into()],
            cwd: None,
            size: Some((40, 4)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    let env = rt(
        &cfg,
        Command::Wait {
            pane: None,
            condition: WaitCondition::Text {
                regex: r"hello-\w+".into(),
            },
            timeout: Duration::from_secs(2),
        },
    )
    .await;
    assert!(
        env.response.success,
        "text-wait should match 'hello-world': {env:?}"
    );
    let _ = rt(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn wait_alt_screen_returns_on_toggle() {
    let (cfg, _h) = boot_daemon().await;
    // Spawn a shell that prints the alt-screen sequence after a brief sleep,
    // so the wait subscribes before the mutation happens.
    let _spawn = rt(
        &cfg,
        Command::Spawn {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                // sleep then print ESC[?1049h then idle so the wait sees the toggle.
                "sleep 0.1; printf '\\033[?1049h'; sleep 5".into(),
            ],
            cwd: None,
            size: Some((40, 4)),
            stdin: agent_tui_protocol::request::StdinMode::default(),
            env: Vec::new(),
        },
    )
    .await;
    let env = rt(
        &cfg,
        Command::Wait {
            pane: None,
            condition: WaitCondition::AltScreen { on: true },
            timeout: Duration::from_secs(2),
        },
    )
    .await;
    assert!(env.response.success, "alt-screen wait: {env:?}");
    assert_eq!(env.response.data.unwrap()["alt_screen"], true);
    let _ = rt(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn wait_hash_unknown_returns_error() {
    let (cfg, _h) = boot_daemon().await;
    spawn_cat(&cfg).await;
    let env = rt(
        &cfg,
        Command::Wait {
            pane: None,
            condition: WaitCondition::Hash {
                hash: "deadbeef".repeat(8),
            },
            timeout: Duration::from_secs(1),
        },
    )
    .await;
    assert!(!env.response.success);
    let err = env.response.error.expect("error");
    assert_eq!(err.code.to_string(), "WAIT_HASH_UNKNOWN");
    let _ = rt(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}

#[tokio::test]
async fn wait_timeout_returns_wait_timeout() {
    let (cfg, _h) = boot_daemon().await;
    spawn_cat(&cfg).await;
    // Wait for a since that's far in the future on a quiet pane.
    let env = rt(
        &cfg,
        Command::Wait {
            pane: None,
            condition: WaitCondition::Since { since: 999_999 },
            timeout: Duration::from_millis(150),
        },
    )
    .await;
    assert!(!env.response.success);
    assert_eq!(
        env.response.error.expect("error").code.to_string(),
        "WAIT_TIMEOUT"
    );
    let _ = rt(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}
