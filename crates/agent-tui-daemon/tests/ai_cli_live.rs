//! Live AI-CLI driving regression tests.
//!
//! These tests use local executables named like common agent harness CLIs so
//! real Claude/Codex/Pi credentials and network are not part of CI. The point
//! is the terminal contract: spawn an AI CLI under a PTY, address its input and
//! response regions, type into the prompt, and wait on rendered state.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_tui_daemon::{DaemonConfig, SocketLayout, run_daemon};
use agent_tui_protocol::request::{SnapshotMode, WaitCondition};
use agent_tui_protocol::{Command, PROTOCOL_VERSION, Request, ResponseEnvelope, SessionId};
use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::traits::tokio::Stream as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;
use uuid::Uuid;

async fn boot() -> (DaemonConfig, agent_tui_daemon::DaemonHandle, PathBuf) {
    let mut sid = Uuid::new_v4().simple().to_string();
    sid.truncate(8);
    let session = SessionId(sid);
    let mut h = Uuid::new_v4().simple().to_string();
    h.truncate(8);
    let root = PathBuf::from(format!("/tmp/at-ai-{h}"));
    std::fs::create_dir_all(&root).expect("mkdir temp root");
    let layout = SocketLayout::for_session_in(&session, root.clone());
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
    (cfg, handle, root)
}

async fn rt(cfg: &DaemonConfig, command: Command) -> ResponseEnvelope {
    let name = agent_tui_daemon::paths::socket_name(&cfg.layout).expect("socket name");
    let stream = Stream::connect(name).await.expect("connect");
    let req = Request {
        id: Uuid::new_v4(),
        protocol: PROTOCOL_VERSION,
        command,
    };
    let mut bytes = serde_json::to_vec(&req).expect("encode request");
    bytes.push(b'\n');
    let (r, mut w) = tokio::io::split(stream);
    w.write_all(&bytes).await.expect("write request");
    let mut lines = BufReader::new(r).lines();
    let line = timeout(Duration::from_secs(30), lines.next_line())
        .await
        .expect("read timeout")
        .expect("read error")
        .expect("eof");
    serde_json::from_str(&line).expect("decode response")
}

fn write_fake_ai_cli(dir: &Path, name: &str) -> PathBuf {
    let bin_dir = dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bin");
    let path = bin_dir.join(name);
    std::fs::write(
        &path,
        format!(
            r#"#!/bin/sh
printf '{name} test shell\n'
printf '> '
while IFS= read -r line; do
  printf '\nthinking about %s\n' "$line"
  sleep 0.05
  printf 'answer: LIVE_AI_CLI_MARKER\n'
  printf '> '
done
"#
        ),
    )
    .expect("write fake ai cli");
    let mut perms = std::fs::metadata(&path)
        .expect("fake ai cli metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod fake ai cli");
    path
}

async fn spawn_fake_ai_cli(cfg: &DaemonConfig, fake_cli: &Path) {
    let spawned = rt(
        cfg,
        Command::Spawn {
            argv: vec![fake_cli.to_string_lossy().into_owned()],
            cwd: None,
            size: Some((60, 8)),
            stdin: agent_tui_protocol::request::StdinMode::Pty,
            env: Vec::new(),
        },
    )
    .await;
    assert!(spawned.response.success, "spawn failed: {spawned:?}");
    let data = spawned.response.data.as_ref().expect("spawn data");
    assert_eq!(data["adapter"], "claude-code");
}

async fn wait_for_input(cfg: &DaemonConfig) {
    let input_ready = rt(
        cfg,
        Command::Wait {
            pane: None,
            condition: WaitCondition::Ref {
                selector: "@ai-cli.input[focused]".into(),
                gone: false,
            },
            timeout: Duration::from_secs(3),
        },
    )
    .await;
    assert!(
        input_ready.response.success,
        "input ref never became ready: {input_ready:?}"
    );
}

async fn type_prompt(cfg: &DaemonConfig) {
    let typed = rt(
        cfg,
        Command::Type {
            pane: None,
            text: "write the marker".into(),
            to: Some("@ai-cli.input".into()),
            lease: None,
        },
    )
    .await;
    assert!(typed.response.success, "type failed: {typed:?}");
    assert!(
        typed.response.data.unwrap()["routed"]
            .as_bool()
            .unwrap_or(false),
        "typing to @ai-cli.input should use routed delivery"
    );
}

async fn submit_prompt(cfg: &DaemonConfig) {
    let submitted = rt(
        cfg,
        Command::Press {
            pane: None,
            keys: "<cr>".into(),
            to: Some("@ai-cli.input".into()),
            lease: None,
        },
    )
    .await;
    assert!(submitted.response.success, "submit failed: {submitted:?}");
}

async fn wait_for_response_marker(cfg: &DaemonConfig) {
    let response_visible = rt(
        cfg,
        Command::Wait {
            pane: None,
            condition: WaitCondition::Ref {
                selector: "@ai-cli.response[name~=/LIVE_AI_CLI_MARKER/]".into(),
                gone: false,
            },
            timeout: Duration::from_secs(5),
        },
    )
    .await;
    assert!(
        response_visible.response.success,
        "response marker never became visible: {response_visible:?}"
    );
}

async fn assert_response_snapshot(cfg: &DaemonConfig) {
    let snap = rt(
        cfg,
        Command::Snapshot {
            pane: None,
            mode: SnapshotMode::Outline,
            png: None,
            annotate: None,
            chrome: None,
            select: Some("@ai-cli.response".into()),
            all: false,
            keep_color: false,
        },
    )
    .await;
    assert!(snap.response.success, "snapshot failed: {snap:?}");
    let data = snap.response.data.unwrap();
    let nodes = data["outline"]["nodes"].as_array().expect("outline nodes");
    assert_eq!(nodes.len(), 1, "expected selected response node: {data:?}");
    assert_eq!(nodes[0]["ref"], "@ai-cli.response");
    assert!(
        nodes[0]["name"]
            .as_str()
            .unwrap_or_default()
            .contains("LIVE_AI_CLI_MARKER"),
        "response node should contain rendered answer: {data:?}"
    );
}

#[tokio::test]
async fn drives_live_ai_sdk_harness_cli_names_by_ai_cli_refs() {
    for name in ["claude", "codex", "pi"] {
        drive_fake_ai_cli(name).await;
    }
}

async fn drive_fake_ai_cli(name: &str) {
    let (cfg, _h, root) = boot().await;
    let fake_cli = write_fake_ai_cli(&root, name);

    spawn_fake_ai_cli(&cfg, &fake_cli).await;
    wait_for_input(&cfg).await;
    type_prompt(&cfg).await;
    submit_prompt(&cfg).await;
    wait_for_response_marker(&cfg).await;
    assert_response_snapshot(&cfg).await;

    let _ = rt(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}
