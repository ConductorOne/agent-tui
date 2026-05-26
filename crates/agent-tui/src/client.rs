//! CLI-side client that opens the daemon's Unix socket and round-trips
//! one JSON request → one JSON response.
//!
//! This is the only path the CLI uses to reach the daemon. When no socket is
//! found we lazily spawn `agent-tui daemon run --session <s>` and retry with
//! a short backoff — mirrors the agent-browser bootstrap.

use std::io::ErrorKind;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use agent_tui_daemon::SocketLayout;
use agent_tui_protocol::{Command, PROTOCOL_VERSION, Request, ResponseEnvelope, SessionId};
use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::timeout;
use tracing::debug;
use uuid::Uuid;

/// Connect, send one request, read one response. Spawns the daemon if no
/// socket is listening.
pub async fn one_shot(layout: &SocketLayout, command: Command) -> Result<ResponseEnvelope> {
    let stream = match connect(&layout.socket).await {
        Ok(s) => s,
        Err(e) if is_unreachable(&e) => {
            debug!(socket = %layout.socket.display(), "daemon unreachable, spawning");
            spawn_daemon(layout)?;
            wait_for_socket(&layout.socket, Duration::from_secs(3)).await?
        }
        Err(e) => return Err(e),
    };
    send_and_recv(stream, command).await
}

async fn connect(socket: &Path) -> Result<UnixStream> {
    UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect to daemon socket {}", socket.display()))
}

fn is_unreachable(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|e| e.downcast_ref::<std::io::Error>())
        .any(|io| {
            matches!(
                io.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            )
        })
}

fn spawn_daemon(layout: &SocketLayout) -> Result<()> {
    let exe = std::env::current_exe().context("current_exe")?;
    let session = layout
        .socket
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default")
        .to_string();
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("--session")
        .arg(&session)
        .arg("--socket-dir")
        .arg(&layout.root);
    // Forward governance settings to the lazily-spawned daemon child. The
    // CLI's `--allowed-binaries` value lives on the *parent* invocation; we
    // propagate it via the env-var binding clap already declares so the
    // daemon process sees the same allowlist.
    if let Ok(csv) = std::env::var("AGENT_TUI_ALLOWED_BINARIES") {
        cmd.env("AGENT_TUI_ALLOWED_BINARIES", csv);
    }
    cmd.arg("daemon")
        .arg("run")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _child = cmd.spawn().context("spawn daemon")?;
    Ok(())
}

async fn wait_for_socket(socket: &Path, max_wait: Duration) -> Result<UnixStream> {
    let deadline = tokio::time::Instant::now() + max_wait;
    loop {
        match UnixStream::connect(socket).await {
            Ok(s) => return Ok(s),
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(e) => bail!(
                "daemon did not bind {} within {}ms: {e}",
                socket.display(),
                max_wait.as_millis()
            ),
        }
    }
}

async fn send_and_recv(mut stream: UnixStream, command: Command) -> Result<ResponseEnvelope> {
    let req = Request {
        id: Uuid::new_v4(),
        protocol: PROTOCOL_VERSION,
        command,
    };
    let mut bytes = serde_json::to_vec(&req)?;
    bytes.push(b'\n');
    stream.write_all(&bytes).await?;

    let (reader, _writer) = stream.split();
    let mut lines = BufReader::new(reader).lines();
    let line = timeout(Duration::from_secs(25), lines.next_line())
        .await
        .context("daemon read timed out (25s)")?
        .context("daemon read failed")?
        .context("daemon closed without responding")?;

    Ok(serde_json::from_str(&line)?)
}

/// Compute the `SocketLayout`, honoring an explicit override.
#[must_use]
pub fn layout_for(session: &str, override_root: Option<&Path>) -> SocketLayout {
    let session_id = SessionId(session.to_string());
    match override_root {
        Some(root) => SocketLayout::for_session_in(&session_id, root.to_path_buf()),
        None => SocketLayout::for_session(&session_id),
    }
}
