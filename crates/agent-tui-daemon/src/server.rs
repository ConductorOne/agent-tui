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
use interprocess::local_socket::ListenerOptions;
use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::traits::tokio::Listener as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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
    /// Optional parent-process PID to monitor. When the parent dies the
    /// daemon shuts itself down within ~500ms. Set automatically by the
    /// CLI's lazy-spawn path; can also be passed via
    /// `agent-tui daemon run --monitor-parent <pid>`.
    pub monitor_parent: Option<u32>,
    /// Idle-shutdown timeout in seconds. The daemon exits after this
    /// many seconds of no client activity. `None` means default
    /// ([`DEFAULT_IDLE_TIMEOUT_SECS`]); `Some(0)` disables idle
    /// shutdown entirely.
    pub idle_timeout_secs: Option<u64>,
}

/// Default idle-shutdown window (15 min). Long enough that an
/// interactive user typing in another terminal doesn't get reaped;
/// short enough that orphaned test daemons go away within a single
/// CI run.
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 900;

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
    /// Unix-epoch seconds of the most recent client request. The idle-
    /// timeout watcher compares this against `now` and shuts the daemon
    /// down when too long has passed. Stored as `u64` in an
    /// `AtomicU64` so handler hot paths can write it lock-free.
    pub last_activity_secs: Arc<std::sync::atomic::AtomicU64>,
    /// Notify channel that triggers daemon shutdown. The `daemon
    /// shutdown` handler fires this; the accept loop watches it.
    pub shutdown: Arc<Notify>,
}

impl DaemonState {
    /// Update the last-activity timestamp to "now". Cheap: one
    /// `AtomicU64::store`. Called from `handle_command` for every
    /// in-bound request.
    pub fn touch_activity(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.last_activity_secs
            .store(now, std::sync::atomic::Ordering::Relaxed);
    }
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
    // **Note on PR_SET_PDEATHSIG (Linux Layer 2 from the cleanup
    // design):** intentionally NOT installed here. PDEATHSIG fires on
    // the death of our *OS* parent, not the *logical* parent the
    // caller wants to monitor. In practice the daemon is forked from
    // an ephemeral `agent-tui spawn ...` CLI that exits seconds later;
    // PDEATHSIG would shut the daemon down right after the first
    // command returns. The `--monitor-parent <pid>` polling monitor
    // below catches "logical parent died" within ~500ms — slower than
    // PDEATHSIG (sub-ms) but correct. If a future caller forks the
    // daemon directly (without an intermediary CLI), it can re-enable
    // PDEATHSIG with a separate explicit opt-in.

    cfg.layout.ensure_root()?;
    // Best-effort: drop any stale socket from a prior daemon at this path.
    // (No-op on Windows where `socket` is just a discovery hint, not the
    // actual named-pipe address.)
    let _ = std::fs::remove_file(&cfg.layout.socket);

    sidecar::write_startup_sidecars(&cfg.layout, &cfg.binary_version, &cfg.engine)?;

    let name = super::paths::socket_name(&cfg.layout)?;
    let listener: Listener = ListenerOptions::new().name(name).create_tokio()?;
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

    // Stash the monitor flags before `cfg` moves into DaemonState.
    let monitor_parent = cfg.monitor_parent;
    let idle_timeout_secs = cfg.idle_timeout_secs.unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let last_activity = Arc::new(std::sync::atomic::AtomicU64::new(now));

    let state = DaemonState {
        cfg,
        registry: Arc::new(Registry::new()),
        generations: Arc::new(handlers::snapshot::GenerationTracker::default()),
        hashes: Arc::new(HashWindow::new()),
        adapters: AdapterRegistry::with_builtins(),
        governance,
        last_activity_secs: last_activity.clone(),
        shutdown: shutdown.clone(),
    };

    // Layer 1: parent-PID monitor. Polls `kill(pid, 0)` every 500ms.
    // If our spawner has exited, fire the shutdown notify.
    if let Some(pid) = monitor_parent {
        let shutdown_for_monitor = shutdown.clone();
        tokio::spawn(parent_pid_monitor(pid, shutdown_for_monitor));
    }

    // Layer 3: idle-timeout watcher. Compares last-activity against
    // now once per minute. `0` disables.
    if idle_timeout_secs > 0 {
        let shutdown_for_idle = shutdown.clone();
        let activity_for_idle = last_activity.clone();
        tokio::spawn(idle_timeout_watcher(
            idle_timeout_secs,
            activity_for_idle,
            shutdown_for_idle,
        ));
    }
    let shutdown_inner = shutdown.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = shutdown_inner.notified() => {
                    info!("shutdown signal received");
                    break;
                }
                accept = listener.accept() => match accept {
                    Ok(sock) => {
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

async fn handle_conn(sock: Stream, state: DaemonState) {
    let (reader, mut writer) = tokio::io::split(sock);
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                // Detect streaming requests early so we can branch
                // to the multi-envelope path. Currently only
                // `Tail { follow: true }` streams.
                let parsed_for_branch: Result<Request, _> = serde_json::from_str(&line);
                let is_streaming = matches!(
                    &parsed_for_branch,
                    Ok(req)
                        if matches!(
                            &req.command,
                            agent_tui_protocol::Command::Tail { follow: true, .. }
                        )
                );
                if is_streaming {
                    if let Err(e) =
                        handle_streaming_tail(&state, parsed_for_branch.unwrap(), &mut writer).await
                    {
                        debug!(error = %e, "streaming tail aborted");
                        return;
                    }
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

/// Stream the child's output to the client as new bytes arrive.
/// Emits one envelope per chunk plus a final `{type:"eof"}` envelope.
///
/// Polling cadence (~50ms) is chosen so:
///   - the response feels live for human-visible output rates
///   - the daemon doesn't spin on idle children
///   - the overhead per empty poll is one mutex-lock + one u64-compare
///
/// Subscribing to the engine's mutation broadcast would be more
/// elegant, but for `tail` we want byte-level deltas (which are
/// upstream of engine mutations), so a polling loop on the output
/// ring is the right primitive.
async fn handle_streaming_tail(
    state: &DaemonState,
    req: Request,
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> std::io::Result<()> {
    use agent_tui_protocol::Command;
    let agent_tui_protocol::request::Command::Tail {
        pane,
        since,
        strip_ansi,
        follow: _,
    } = req.command
    else {
        // Already guarded; defensive bail.
        return Ok(());
    };
    let _ = Command::Tail {
        pane: pane.clone(),
        since,
        strip_ansi,
        follow: false,
    };
    state.touch_activity();
    let pane_arc = match crate::pane::resolve_focused(&state.registry, pane.clone()).await {
        Ok(p) => p,
        Err(resp) => {
            let env = wrap_envelope(state, req.id, resp);
            return write_envelope(writer, &env).await;
        }
    };

    let mut cursor: u64 = since;
    let poll_interval = tokio::time::Duration::from_millis(50);
    loop {
        // Mark activity each tick — a live `tail --follow` shouldn't
        // count as idle.
        state.touch_activity();
        let read = pane_arc.pty.tail(cursor);
        if !read.bytes.is_empty() {
            let payload = if strip_ansi {
                let text = strip_ansi_for_streaming(&read.bytes);
                serde_json::json!({
                    "type": "chunk",
                    "pane": pane_arc.id,
                    "text": text,
                    "next_since": read.total,
                    "lost_bytes": read.lost_bytes,
                })
            } else {
                use base64::Engine as _;
                let encoded = base64::engine::general_purpose::STANDARD.encode(&read.bytes);
                serde_json::json!({
                    "type": "chunk",
                    "pane": pane_arc.id,
                    "bytes_b64": encoded,
                    "next_since": read.total,
                    "lost_bytes": read.lost_bytes,
                })
            };
            let env = wrap_envelope(state, req.id, Response::ok(payload));
            write_envelope(writer, &env).await?;
            cursor = read.total;
        }
        // Check child exit AFTER reading any final bytes the child wrote
        // on its way out. The child can be reaped (`try_wait` → exited) a
        // beat before the reader thread flushes its last bytes into the
        // ring, so we only emit `eof` once the reader has actually drained
        // the PTY (`reader_finished`). Until then we keep polling so the
        // next iteration streams those trailing bytes — otherwise a
        // fast-exiting `watch`/`tail --follow` child loses its output.
        if pane_arc.pty.try_exit_code().ok().flatten().is_some() && pane_arc.pty.reader_finished() {
            // Final drain: emit anything the reader flushed since the last
            // read before the terminal envelope.
            let tail = pane_arc.pty.tail(cursor);
            if !tail.bytes.is_empty() {
                let payload = if strip_ansi {
                    let text = strip_ansi_for_streaming(&tail.bytes);
                    serde_json::json!({
                        "type": "chunk",
                        "pane": pane_arc.id,
                        "text": text,
                        "next_since": tail.total,
                        "lost_bytes": tail.lost_bytes,
                    })
                } else {
                    use base64::Engine as _;
                    let encoded = base64::engine::general_purpose::STANDARD.encode(&tail.bytes);
                    serde_json::json!({
                        "type": "chunk",
                        "pane": pane_arc.id,
                        "bytes_b64": encoded,
                        "next_since": tail.total,
                        "lost_bytes": tail.lost_bytes,
                    })
                };
                let env = wrap_envelope(state, req.id, Response::ok(payload));
                write_envelope(writer, &env).await?;
                cursor = tail.total;
            }
            let payload = serde_json::json!({
                "type": "eof",
                "pane": pane_arc.id,
                "next_since": cursor,
                "exit_code": pane_arc.pty.try_exit_code().ok().flatten(),
            });
            let env = wrap_envelope(state, req.id, Response::ok(payload));
            return write_envelope(writer, &env).await;
        }
        tokio::time::sleep(poll_interval).await;
    }
}

/// Build a `ResponseEnvelope` mirroring `dispatch`'s shape but without
/// the `tool_output_delim` (streaming chunks don't need per-chunk
/// nonces; the consuming agent should grant trust to the stream as
/// a whole).
fn wrap_envelope(state: &DaemonState, id: uuid::Uuid, response: Response) -> ResponseEnvelope {
    ResponseEnvelope {
        id,
        protocol: PROTOCOL_VERSION,
        version: state.cfg.binary_version.clone(),
        session: Some(state.cfg.session.clone()),
        pane: None,
        generation: None,
        sequence: None,
        elapsed_ms: 0,
        tool_output_delim: None,
        response,
    }
}

async fn write_envelope(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    env: &ResponseEnvelope,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(env).map_err(std::io::Error::other)?;
    writer.write_all(&bytes).await?;
    writer.write_all(b"\n").await?;
    Ok(())
}

/// Same ANSI-stripping logic as the one-shot `tail` handler. Kept
/// inline here so the streaming path doesn't depend on handler
/// module visibility.
fn strip_ansi_for_streaming(bytes: &[u8]) -> String {
    // Delegate to the existing implementation via the public path
    // (call into the tail handler indirectly by simulating a non-
    // follow tail and re-using its result). The simpler thing is to
    // call into the same internal function — exposed via a small
    // pub(crate) shim.
    crate::handlers::raw::strip_ansi_for_streaming(bytes)
}

async fn dispatch(state: &DaemonState, line: &str) -> ResponseEnvelope {
    // Mark this moment as "active" so the idle-timeout watcher
    // doesn't shut us down mid-flight. Cheap (one atomic store).
    state.touch_activity();
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
        Command::Stdin { .. } => "stdin",
        Command::CloseStdin { .. } => "close_stdin",
        Command::Tail { .. } => "tail",
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
        | Command::Stdin { pane, .. }
        | Command::CloseStdin { pane, .. }
        | Command::Tail { pane, .. }
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
        Command::Spawn {
            argv,
            cwd,
            size,
            stdin,
            env,
        } => {
            handlers::spawn::run(
                &state.cfg.session,
                &state.registry,
                &state.adapters,
                &state.governance,
                argv,
                cwd,
                size,
                stdin,
                env,
            )
            .await
        }
        Command::Die { pane } => handlers::die::run(&state.registry, pane).await,
        Command::List { all } => handlers::list::run(&state.registry, all).await,
        Command::Snapshot {
            pane,
            mode,
            select,
            all,
            keep_color,
            ..
        } => {
            handlers::snapshot::run(
                &state.registry,
                &state.generations,
                &state.hashes,
                pane,
                handlers::snapshot::SnapshotParams {
                    mode,
                    select,
                    all,
                    keep_color,
                },
            )
            .await
        }
        Command::Wait {
            pane,
            condition,
            timeout,
        } => handlers::wait::run(&state.registry, &state.hashes, pane, condition, timeout).await,
        Command::Press { pane, keys, to } => {
            handlers::input::press(&state.registry, &state.governance, pane, keys, to).await
        }
        Command::Type { pane, text, to } => {
            handlers::input::type_text(&state.registry, &state.governance, pane, text, to).await
        }
        Command::SendAnsi { pane, bytes_hex } => {
            handlers::raw::send_ansi(&state.registry, pane, bytes_hex).await
        }
        Command::Stdin { pane, bytes_hex } => {
            handlers::raw::stdin(&state.registry, pane, bytes_hex).await
        }
        Command::CloseStdin { pane } => handlers::raw::close_stdin(&state.registry, pane).await,
        Command::Tail {
            pane,
            since,
            strip_ansi,
            follow: _, // streaming follow is dispatched above; this is the one-shot path
        } => handlers::raw::tail(&state.registry, pane, since, strip_ansi).await,
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
            // Fire the shutdown notify. The accept loop wakes up,
            // closes the listener, and the daemon exits. We respond
            // BEFORE notifying so the client gets the ack — otherwise
            // the daemon could tear down the socket mid-response.
            //
            // `notify_waiters` wakes all current and future waiters
            // (the accept loop has multiple await points on this).
            let shutdown = state.shutdown.clone();
            tokio::spawn(async move {
                // Tiny defer so this response flushes before the
                // socket goes away.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                shutdown.notify_waiters();
            });
            Response::ok(serde_json::json!({
                "status": "shutting_down",
            }))
        }
        Command::Focus { pane } => handlers::focus::run(&state.registry, pane).await,
        Command::Eval { .. } => Response::err(ErrorBody::new(
            ErrorCode::Internal,
            "eval not yet wired (lands with adapters in P2)",
            "see docs/RFC.md §17 for the roadmap",
        )),
    }
}

/// Polls `kill(pid, 0)` every 500ms; fires the shutdown notify when the
/// PID is gone. Used by the `--monitor-parent` flag.
///
/// Cross-platform: `kill(pid, 0)` returns success while the process
/// exists, `ESRCH` after it dies. On Windows we'd shell out to
/// `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)` — but
/// the daemon's Windows path is still in design (RFC §13.x), so this
/// helper is Unix-only for now and silently does nothing elsewhere.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
async fn parent_pid_monitor(pid: u32, shutdown: Arc<Notify>) {
    #[cfg(unix)]
    {
        use nix::sys::signal;
        use nix::unistd::Pid;
        let target = Pid::from_raw(pid as i32);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            // `kill(pid, None)` is `kill(pid, 0)` — error iff the
            // process is gone (ESRCH) or we lack permission (EPERM,
            // which also means "we can't see it" — treat as dead).
            if signal::kill(target, None).is_err() {
                info!(monitor_parent = pid, "parent exited; shutting down daemon");
                shutdown.notify_waiters();
                break;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, shutdown);
    }
}

/// Watcher task that fires shutdown when the daemon has been idle for
/// `timeout_secs`. Wakes up once every `min(timeout/4, 60)` seconds so
/// the check is granular without burning CPU on long timeouts.
async fn idle_timeout_watcher(
    timeout_secs: u64,
    last_activity: Arc<std::sync::atomic::AtomicU64>,
    shutdown: Arc<Notify>,
) {
    let tick = std::time::Duration::from_secs((timeout_secs / 4).clamp(1, 60));
    loop {
        tokio::time::sleep(tick).await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let last = last_activity.load(std::sync::atomic::Ordering::Relaxed);
        if now.saturating_sub(last) >= timeout_secs {
            info!(
                idle_timeout_secs = timeout_secs,
                "idle timeout reached; shutting down daemon"
            );
            shutdown.notify_waiters();
            break;
        }
    }
}

// `install_parent_death_signal` was removed — see the comment at the
// top of `run_daemon` for why PDEATHSIG is unsafe in the default
// lazy-spawn topology. If a future caller wants it back, it would
// look roughly like:
//
//     let _ = nix::sys::prctl::set_pdeathsig(
//         Some(nix::sys::signal::Signal::SIGTERM),
//     );
