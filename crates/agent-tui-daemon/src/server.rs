//! The daemon's tokio server loop.
//!
//! Binds the Unix-domain socket from [`SocketLayout`], handles
//! version-handshake, and dispatches every line of JSON it receives to
//! [`handle_command`]. v0.1.0 dispatch is a stub matrix — real engine /
//! adapter / recorder wiring comes in P0–P2.

use std::sync::Arc;
use std::time::Instant;

use agent_tui_protocol::{
    ErrorBody, ErrorCode, PROTOCOL_VERSION, Request, Response, ResponseEnvelope, SessionId,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

use super::paths::SocketLayout;
use super::sidecar;

/// How the daemon is configured at startup.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Session id this daemon will own.
    pub session: SessionId,
    /// Where to bind the socket + sidecars.
    pub layout: SocketLayout,
    /// Engine name reported in the sidecar (`wezterm`, `alacritty`).
    pub engine: String,
    /// Binary semver string.
    pub binary_version: String,
}

/// Handle returned by [`run_daemon`]. Currently only carries a shutdown
/// signal; `wait` on `.shutdown_notified()` to know when the loop exits.
#[derive(Debug, Clone)]
pub struct DaemonHandle {
    /// Notify channel fired when the daemon is about to exit.
    pub shutdown: Arc<Notify>,
}

/// Start the daemon. Blocks on the accept loop until `shutdown` is fired.
///
/// # Errors
/// IO errors binding the socket or writing sidecars.
pub async fn run_daemon(cfg: DaemonConfig) -> std::io::Result<DaemonHandle> {
    cfg.layout.ensure_root()?;
    // Best-effort: drop any stale socket from a prior daemon at this path.
    let _ = std::fs::remove_file(&cfg.layout.socket);

    sidecar::write_startup_sidecars(&cfg.layout, &cfg.binary_version, &cfg.engine)?;

    let listener = UnixListener::bind(&cfg.layout.socket)?;
    info!(
        socket = %cfg.layout.socket.display(),
        session = %cfg.session,
        version = %cfg.binary_version,
        engine = %cfg.engine,
        protocol = PROTOCOL_VERSION,
        "agent-tui daemon listening"
    );

    let shutdown = Arc::new(Notify::new());
    let handle = DaemonHandle {
        shutdown: shutdown.clone(),
    };

    let cfg = Arc::new(cfg);
    let shutdown_inner = shutdown.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = shutdown_inner.notified() => {
                    info!("shutdown signal received");
                    break;
                }
                accept = listener.accept() => match accept {
                    Ok((sock, _addr)) => {
                        let cfg = cfg.clone();
                        tokio::spawn(handle_conn(sock, cfg));
                    }
                    Err(e) => {
                        error!(error = %e, "accept error");
                    }
                }
            }
        }
        sidecar::remove_all_sidecars(&cfg.layout);
    });

    Ok(handle)
}

async fn handle_conn(sock: UnixStream, cfg: Arc<DaemonConfig>) {
    let (reader, mut writer) = sock.into_split();
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                let response = dispatch(&cfg, &line).await;
                let bytes = match serde_json::to_vec(&response) {
                    Ok(b) => b,
                    Err(e) => {
                        error!(error = %e, "failed to encode response");
                        continue;
                    }
                };
                if let Err(e) = writer.write_all(&bytes).await {
                    debug!(error = %e, "client gone mid-write");
                    return;
                }
                if let Err(e) = writer.write_all(b"\n").await {
                    debug!(error = %e, "client gone after newline");
                    return;
                }
            }
            Ok(None) => return,
            Err(e) => {
                warn!(error = %e, "read error");
                return;
            }
        }
    }
}

async fn dispatch(cfg: &DaemonConfig, line: &str) -> ResponseEnvelope {
    let start = Instant::now();
    let parsed: Result<Request, _> = serde_json::from_str(line);
    let id = match &parsed {
        Ok(r) => r.id,
        Err(_) => uuid::Uuid::nil(),
    };

    let response = match parsed {
        Ok(req) => {
            if req.protocol == PROTOCOL_VERSION {
                handle_command(cfg, req.command).await
            } else {
                Response::err(ErrorBody::new(
                    ErrorCode::DaemonVersionDrift,
                    format!(
                        "client protocol={}, daemon protocol={}",
                        req.protocol, PROTOCOL_VERSION
                    ),
                    "ensure CLI and daemon are from the same release",
                ))
            }
        }
        Err(e) => Response::err(ErrorBody::new(
            ErrorCode::InvalidArgs,
            format!("malformed request: {e}"),
            "send a single JSON request per line",
        )),
    };

    ResponseEnvelope {
        id,
        protocol: PROTOCOL_VERSION,
        version: cfg.binary_version.clone(),
        session: Some(cfg.session.clone()),
        pane: None,
        generation: None,
        sequence: None,
        elapsed_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        tool_output_delim: None,
        response,
    }
}

// Async is intentional: P0–P2 will introduce engine/adapter awaits inside
// every arm. The clippy lint is right today but wrong tomorrow.
#[allow(clippy::unused_async)]
async fn handle_command(_cfg: &DaemonConfig, cmd: agent_tui_protocol::Command) -> Response {
    use agent_tui_protocol::Command;
    // v0.1.0 scaffolding — every command returns a typed "not yet" error
    // except DaemonStatus and DaemonShutdown which are useful immediately.
    match cmd {
        Command::DaemonStatus => Response::ok(serde_json::json!({
            "status": "running",
            "protocol": PROTOCOL_VERSION,
        })),
        Command::DaemonShutdown { force: _ } => {
            // P0 follow-up: actually shut down. For now, just acknowledge.
            Response::ok(serde_json::json!({ "queued": true }))
        }
        _ => Response::err(ErrorBody::new(
            ErrorCode::Internal,
            "command not yet wired in this scaffolding build",
            "see docs/RFC.md §17 for the roadmap",
        )),
    }
}
