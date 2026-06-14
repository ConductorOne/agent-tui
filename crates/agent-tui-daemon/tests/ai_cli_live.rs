//! Live agent-driving-agent regression tests.
//!
//! These tests use local executables named like common AI CLIs so real
//! credentials and network are not part of CI. The point is the terminal-screen
//! control contract: spawn an AI CLI-shaped process under a PTY, address its
//! provider-specific input/response/state refs, type into the prompt, and wait
//! on rendered state.

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

fn write_fake_long_agent(dir: &Path, name: &str) -> PathBuf {
    let bin_dir = dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bin");
    let path = bin_dir.join(name);
    std::fs::write(
        &path,
        format!(
            r#"#!/bin/sh
printf '{name} long-session fixture\n'
printf '> '
turn=0
while IFS= read -r line; do
  turn=$((turn + 1))
  case "$turn" in
    1)
      printf '\nplan:\n'
      printf '1. write release-note.md\n'
      printf '2. run check-release.sh\n'
      printf 'approval: type approve to continue\n'
      printf '> '
      ;;
    2)
      if [ "$line" != "approve" ]; then
        printf '\napproval: type approve to continue\n'
        printf '> '
        continue
      fi
      cat > release-note.md <<'NOTE'
# Agents controlling agents
An outer agent supervised an inner agent through a terminal session.
It waited for refs, approved the plan, verified the edit, and ran checks.
NOTE
      printf '\nstatus: file-change release-note.md\n'
      printf '> '
      ;;
    3)
      printf '\ncommand: ./check-release.sh\n'
      if sh ./check-release.sh > check-report.txt; then
        cat check-report.txt
        printf 'status: done\n'
      else
        printf 'status: failed\n'
      fi
      printf '> '
      ;;
    *)
      printf '\nstatus: done\n'
      printf '> '
      ;;
  esac
done
"#
        ),
    )
    .expect("write fake long agent");
    let mut perms = std::fs::metadata(&path)
        .expect("fake long agent metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod fake long agent");
    path
}

async fn spawn_fake_ai_cli(
    cfg: &DaemonConfig,
    fake_cli: &Path,
    cwd: Option<&Path>,
    expected_adapter: &str,
) {
    let spawned = rt(
        cfg,
        Command::Spawn {
            argv: vec![fake_cli.to_string_lossy().into_owned()],
            cwd: cwd.map(|p| p.to_string_lossy().into_owned()),
            size: Some((60, 8)),
            stdin: agent_tui_protocol::request::StdinMode::Pty,
            env: Vec::new(),
        },
    )
    .await;
    assert!(spawned.response.success, "spawn failed: {spawned:?}");
    let data = spawned.response.data.as_ref().expect("spawn data");
    assert_eq!(data["adapter"], expected_adapter);
}

async fn wait_for_input(cfg: &DaemonConfig, root_ref: &str) {
    wait_for_ref(
        cfg,
        &format!("{root_ref}.input[focused]"),
        Duration::from_secs(3),
    )
    .await;
}

async fn wait_for_ref(cfg: &DaemonConfig, selector: &str, max: Duration) {
    let input_ready = rt(
        cfg,
        Command::Wait {
            pane: None,
            condition: WaitCondition::Ref {
                selector: selector.into(),
                gone: false,
            },
            timeout: max,
        },
    )
    .await;
    assert!(
        input_ready.response.success,
        "ref {selector:?} never became ready: {input_ready:?}"
    );
}

async fn type_prompt(cfg: &DaemonConfig, root_ref: &str, text: &str) {
    let typed = rt(
        cfg,
        Command::Type {
            pane: None,
            text: text.into(),
            to: Some(format!("{root_ref}.input")),
            lease: None,
        },
    )
    .await;
    assert!(typed.response.success, "type failed: {typed:?}");
    assert!(
        typed.response.data.unwrap()["routed"]
            .as_bool()
            .unwrap_or(false),
        "typing to {root_ref}.input should use routed delivery"
    );
}

async fn submit_prompt(cfg: &DaemonConfig, root_ref: &str) {
    let submitted = rt(
        cfg,
        Command::Press {
            pane: None,
            keys: "<cr>".into(),
            to: Some(format!("{root_ref}.input")),
            lease: None,
        },
    )
    .await;
    assert!(submitted.response.success, "submit failed: {submitted:?}");
}

async fn wait_for_response_marker(cfg: &DaemonConfig, root_ref: &str) {
    wait_for_ref(
        cfg,
        &format!("{root_ref}.response[name~=/LIVE_AI_CLI_MARKER/]"),
        Duration::from_secs(5),
    )
    .await;
}

async fn assert_response_snapshot(cfg: &DaemonConfig, root_ref: &str) {
    let snap = rt(
        cfg,
        Command::Snapshot {
            pane: None,
            mode: SnapshotMode::Outline,
            png: None,
            annotate: None,
            chrome: None,
            select: Some(format!("{root_ref}.response")),
            all: false,
            keep_color: false,
        },
    )
    .await;
    assert!(snap.response.success, "snapshot failed: {snap:?}");
    let data = snap.response.data.unwrap();
    let nodes = data["outline"]["nodes"].as_array().expect("outline nodes");
    assert_eq!(nodes.len(), 1, "expected selected response node: {data:?}");
    let expected_ref = format!("{root_ref}.response");
    assert_eq!(nodes[0]["ref"].as_str(), Some(expected_ref.as_str()));
    assert!(
        nodes[0]["name"]
            .as_str()
            .unwrap_or_default()
            .contains("LIVE_AI_CLI_MARKER"),
        "response node should contain rendered answer: {data:?}"
    );
}

#[tokio::test]
async fn drives_provider_specific_agent_region_contract_for_fake_known_binaries() {
    for (bin, adapter, root_ref) in [
        ("claude", "claude-code", "@claude"),
        ("codex", "codex", "@codex"),
        ("pi", "pi", "@pi"),
    ] {
        drive_fake_ai_cli(bin, adapter, root_ref).await;
    }
}

async fn drive_fake_ai_cli(name: &str, expected_adapter: &str, root_ref: &str) {
    let (cfg, _h, root) = boot().await;
    let fake_cli = write_fake_ai_cli(&root, name);

    spawn_fake_ai_cli(&cfg, &fake_cli, None, expected_adapter).await;
    wait_for_input(&cfg, root_ref).await;
    type_prompt(&cfg, root_ref, "write the marker").await;
    submit_prompt(&cfg, root_ref).await;
    wait_for_response_marker(&cfg, root_ref).await;
    assert_response_snapshot(&cfg, root_ref).await;

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
async fn drives_long_running_inner_agent_to_file_change_and_done() {
    let (cfg, _h, root) = boot().await;
    let fake_cli = write_fake_long_agent(&root, "codex");
    let work = root.join("work");
    std::fs::create_dir_all(&work).expect("mkdir work");
    std::fs::write(
        work.join("check-release.sh"),
        "#!/bin/sh\ngrep -q 'outer agent supervised' release-note.md && echo 'check-release: pass'\n",
    )
    .expect("write check-release");

    spawn_fake_ai_cli(&cfg, &fake_cli, Some(&work), "codex").await;
    wait_for_input(&cfg, "@codex").await;
    type_prompt(
        &cfg,
        "@codex",
        "plan the release note and wait for approval",
    )
    .await;
    submit_prompt(&cfg, "@codex").await;
    wait_for_ref(&cfg, "@codex.approval", Duration::from_secs(5)).await;

    wait_for_input(&cfg, "@codex").await;
    type_prompt(&cfg, "@codex", "approve").await;
    submit_prompt(&cfg, "@codex").await;
    wait_for_ref(&cfg, "@codex.file-change", Duration::from_secs(5)).await;

    let note = std::fs::read_to_string(work.join("release-note.md")).expect("release note");
    assert!(
        note.contains("outer agent supervised"),
        "inner agent did not write expected note: {note:?}"
    );

    wait_for_input(&cfg, "@codex").await;
    type_prompt(&cfg, "@codex", "run checks").await;
    submit_prompt(&cfg, "@codex").await;
    wait_for_ref(&cfg, "@codex.done", Duration::from_secs(5)).await;

    let report = std::fs::read_to_string(work.join("check-report.txt")).expect("check report");
    assert_eq!(report.trim(), "check-release: pass");

    let _ = rt(
        &cfg,
        Command::Die {
            pane: None,
            grace: None,
        },
    )
    .await;
}
