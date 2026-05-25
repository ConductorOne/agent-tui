//! CLI-side client that opens the daemon's Unix socket and round-trips
//! one JSON request → one JSON response.
//!
//! This is the only path the CLI uses to reach the daemon. Spawning the
//! daemon when no socket is found is handled here too.

use std::path::Path;
use std::time::Duration;

use agent_tui_daemon::SocketLayout;
use agent_tui_protocol::{Command, PROTOCOL_VERSION, Request, ResponseEnvelope, SessionId};
use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::timeout;
use uuid::Uuid;

/// Connect, send one request, read one response.
///
/// If no daemon is listening on the layout's socket, returns the underlying
/// I/O error so the caller can decide whether to spawn one.
pub async fn one_shot(layout: &SocketLayout, command: Command) -> Result<ResponseEnvelope> {
    let mut stream = UnixStream::connect(&layout.socket)
        .await
        .with_context(|| format!("connect to daemon socket {}", layout.socket.display()))?;

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
