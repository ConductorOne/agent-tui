//! The daemon's tokio server loop.
//!
//! Binds the Unix-domain socket from [`SocketLayout`], handles
//! version-handshake, and dispatches every line of JSON it receives to
//! [`handle_command`].

use std::sync::Arc;
use std::time::Instant;

use agent_tui_protocol::{
    ErrorBody, ErrorCode, PROTOCOL_VERSION, Request, Response, ResponseEnvelope, SessionId,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

use super::adapter_registry::AdapterRegistry;
use super::governance::Governance;
use super::handlers;
use super::hash_window::HashWindow;
use super::pane::Registry;
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
    /// Optional binary allowlist (CSV form). Empty / None == permissive.
    pub allowed_binaries: Option<String>,
}

fn build_governance(cfg: &DaemonConfig) -> Governance {
    use super::governance::{AllowlistEvaluator, Governance};
    if let Some(csv) = cfg.allowed_binaries.as_ref() {
        Governance::new(std::sync::Arc::new(AllowlistEvaluator::from_csv(csv)))
    } else {
        Governance::allow_all()
    }
}

/// Per-process daemon state. Wraps the immutable [`DaemonConfig`] alongside
/// the shared, mutable resources every connection handler needs.
#[derive(Clone)]
pub struct DaemonState {
    /// Immutable config captured at daemon launch.
    pub cfg: DaemonConfig,
    /// Pane registry shared across every connection.
    pub registry: Arc<Registry>,
    /// Per-pane snapshot-generation tracker.
    pub generations: Arc<handlers::snapshot::GenerationTracker>,
    /// Per-pane (sequence -> hash) ring backing `wait --hash`.
    pub hashes: Arc<HashWindow>,
    /// Available adapter implementations (built-in + plug-ins).
    pub adapters: AdapterRegistry,
    /// Typed-Action interceptor + audit firehose.
    pub governance: Governance,
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

    let governance = build_governance(&cfg);
    let state = DaemonState {
        cfg,
        registry: Arc::new(Registry::new()),
        generations: Arc::new(handlers::snapshot::GenerationTracker::default()),
        hashes: Arc::new(HashWindow::new()),
        adapters: AdapterRegistry::with_builtins(),
        governance,
    };
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
                        let state = state.clone();
                        tokio::spawn(handle_conn(sock, state));
                    }
                    Err(e) => {
                        error!(error = %e, "accept error");
                    }
                }
            }
        }
        sidecar::remove_all_sidecars(&state.cfg.layout);
    });

    Ok(handle)
}

async fn handle_conn(sock: UnixStream, state: DaemonState) {
    let (reader, mut writer) = sock.into_split();
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                let response = dispatch(&state, &line).await;
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

async fn dispatch(state: &DaemonState, line: &str) -> ResponseEnvelope {
    let start = Instant::now();
    let parsed: Result<Request, _> = serde_json::from_str(line);
    let id = match &parsed {
        Ok(r) => r.id,
        Err(_) => uuid::Uuid::nil(),
    };
    let needs_delim = parsed
        .as_ref()
        .ok()
        .is_some_and(|r| carries_pty_bytes(&r.command));

    let response = match parsed {
        Ok(req) => {
            if req.protocol == PROTOCOL_VERSION {
                handle_command(state, req.command).await
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

    let tool_output_delim = if needs_delim && response.success {
        Some(agent_tui_protocol::ToolOutputDelim::from_nonce(
            &fresh_nonce(),
        ))
    } else {
        None
    };

    ResponseEnvelope {
        id,
        protocol: PROTOCOL_VERSION,
        version: state.cfg.binary_version.clone(),
        session: Some(state.cfg.session.clone()),
        pane: None,
        generation: None,
        sequence: None,
        elapsed_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        tool_output_delim,
        response,
    }
}

/// Does this command's response carry PTY-origin bytes that an agent might
/// otherwise confuse for trusted output? Snapshots are the obvious one;
/// `get text` and `scroll history` land here once they're wired.
fn carries_pty_bytes(cmd: &agent_tui_protocol::Command) -> bool {
    use agent_tui_protocol::Command;
    matches!(cmd, Command::Snapshot { .. })
}

/// 8 hex chars (32 bits) of cryptographic-grade randomness per response.
/// `rand::thread_rng()` defaults to `ChaCha12` which is fine for this — the
/// only attacker model is "untrusted TUI bytes" and they cannot observe our
/// RNG state from inside the PTY.
fn fresh_nonce() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 4];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

async fn handle_command(state: &DaemonState, cmd: agent_tui_protocol::Command) -> Response {
    let pane_hint = pane_hint_of(&cmd);
    let op_name = op_name_of(&cmd);
    let response = dispatch_command(state, cmd).await;
    if let Some(id) = pane_hint
        && let Some(pane) = state.registry.get(&id).await
        && let Some(rec) = pane.pty.recorder()
    {
        let err = response.error.as_ref().map(|e| e.code.to_string());
        rec.push_marker(op_name, op_name, response.success, err.as_deref());
    }
    response
}

fn op_name_of(cmd: &agent_tui_protocol::Command) -> &'static str {
    use agent_tui_protocol::Command;
    match cmd {
        Command::Spawn { .. } => "spawn",
        Command::List { .. } => "list",
        Command::Snapshot { .. } => "snapshot",
        Command::Press { .. } => "press",
        Command::Type { .. } => "type",
        Command::SendAnsi { .. } => "send_ansi",
        Command::Resize { .. } => "resize",
        Command::Signal { .. } => "signal",
        Command::Die { .. } => "die",
        Command::Wait { .. } => "wait",
        Command::Eval { .. } => "eval",
        Command::Focus { .. } => "focus",
        Command::DaemonStatus => "daemon_status",
        Command::DaemonShutdown { .. } => "daemon_shutdown",
    }
}

fn pane_hint_of(cmd: &agent_tui_protocol::Command) -> Option<agent_tui_protocol::PaneId> {
    use agent_tui_protocol::Command;
    match cmd {
        Command::Snapshot { pane, .. }
        | Command::Press { pane, .. }
        | Command::Type { pane, .. }
        | Command::SendAnsi { pane, .. }
        | Command::Resize { pane, .. }
        | Command::Signal { pane, .. }
        | Command::Die { pane }
        | Command::Wait { pane, .. }
        | Command::Eval { pane, .. } => pane.clone(),
        // Spawn fires after the pane id is allocated; the spawn handler itself
        // pushes its own marker. Daemon/Focus/List/Status are session-wide.
        _ => None,
    }
}

async fn dispatch_command(state: &DaemonState, cmd: agent_tui_protocol::Command) -> Response {
    use agent_tui_protocol::Command;
    match cmd {
        Command::Spawn { argv, cwd, size } => {
            handlers::spawn::run(
                &state.cfg.session,
                &state.registry,
                &state.adapters,
                &state.governance,
                argv,
                cwd,
                size,
            )
            .await
        }
        Command::Die { pane } => handlers::die::run(&state.registry, pane).await,
        Command::List { all } => handlers::list::run(&state.registry, all).await,
        Command::Snapshot { pane, mode, .. } => {
            handlers::snapshot::run(
                &state.registry,
                &state.generations,
                &state.hashes,
                pane,
                mode,
            )
            .await
        }
        Command::Wait {
            pane,
            condition,
            timeout,
        } => handlers::wait::run(&state.registry, &state.hashes, pane, condition, timeout).await,
        Command::Press { pane, keys } => {
            handlers::input::press(&state.registry, &state.governance, pane, keys).await
        }
        Command::Type { pane, text } => {
            handlers::input::type_text(&state.registry, &state.governance, pane, text).await
        }
        Command::SendAnsi { pane, bytes_hex } => {
            handlers::raw::send_ansi(&state.registry, pane, bytes_hex).await
        }
        Command::Resize { pane, cols, rows } => {
            handlers::raw::resize(&state.registry, pane, cols, rows).await
        }
        Command::Signal { pane, signal } => {
            handlers::signal::run(&state.registry, pane, signal).await
        }
        Command::DaemonStatus => Response::ok(serde_json::json!({
            "status": "running",
            "protocol": PROTOCOL_VERSION,
            "panes": state.registry.count().await,
        })),
        Command::DaemonShutdown { force: _ } => {
            // P0 follow-up: actually shut down. For now, just acknowledge.
            Response::ok(serde_json::json!({ "queued": true }))
        }
        Command::Focus { pane } => handlers::focus::run(&state.registry, pane).await,
        Command::Eval { .. } => Response::err(ErrorBody::new(
            ErrorCode::Internal,
            "eval not yet wired (lands with adapters in P2)",
            "see docs/RFC.md §17 for the roadmap",
        )),
    }
}
