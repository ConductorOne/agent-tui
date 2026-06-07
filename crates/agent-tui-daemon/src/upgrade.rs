//! In-place daemon upgrade — Option A (same-PID re-exec).
//!
//! The daemon owns each pane's PTY master fd and is the PARENT of the
//! (`setsid`'d) child harness. An `execve(2)` into the same PID keeps that
//! parentage intact: `waitpid` still works on the new image, so exit-code
//! fidelity (137 / 143 / N) is preserved *for free*. `execve` nukes the
//! thread pool and the heap, so the only thing we have to carry across the
//! swap is a little in-memory state — which we serialize into a handoff blob.
//!
//! ## The handoff
//!
//! 1. **fd lifecycle.** PTY masters are `O_CLOEXEC` by default (portable-pty
//!    sets it). Right before `execve` we clear `FD_CLOEXEC` on each live
//!    pane's master fd so the new image inherits it *by number*.
//! 2. **`.cast` durability.** We `fsync` each pane's `.cast` recording so no
//!    consumed-but-unpersisted output is lost — the recording IS the
//!    checkpoint the new image replays.
//! 3. **The blob.** A small JSON [`Handoff`] (pane table: id → master fd
//!    number, child pid, geometry, last exit, lease, cast path, ring
//!    high-water, …) is passed across the exec as a hidden `daemon run
//!    --adopt-handoff <json>` CLI arg — the KISS channel: the fds travel by
//!    number, the blob just names them. A CLI arg (vs an env var) is *not*
//!    inherited by the new daemon's PTY children, so the blob never leaks.
//!
//! ## Adopt-on-startup
//!
//! When a fresh daemon image is launched with a handoff blob
//! ([`DaemonState::cfg`]`.adopt_handoff`), it [`adopt`]s each pane: takes the
//! inherited master fd, **replays the `.cast` to rebuild the alacritty grid**,
//! resumes the reader loop, restores last-exit / lease / geometry, and
//! re-binds the listen socket at the same path.
//!
//! ## Scope (U1, level L1)
//!
//! One pane, "session survives", exit fidelity intact. Viewer reconnect (L2)
//! and N-panes-at-scale / rotated-cast replay (L3) are deferred to U2/U3.

use std::path::PathBuf;

use agent_tui_protocol::{ErrorBody, ErrorCode, Response};
use serde::{Deserialize, Serialize};
use tracing::error;

use crate::server::DaemonState;

// Unix-only imports — the whole re-exec/adopt mechanism is Unix-only (the
// Windows daemon process model is still in design). Gating these keeps the
// Windows `cargo test --workspace` lane warning-free under `-D warnings`.
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use agent_tui_adapter::PaneInfo;
#[cfg(unix)]
use agent_tui_engine::Engine;
#[cfg(unix)]
use agent_tui_engine_alacritty::AlacrittyEngine;
#[cfg(unix)]
use agent_tui_protocol::PaneId;
#[cfg(unix)]
use agent_tui_recorder::{Recorder, RecorderConfig};
#[cfg(unix)]
use tracing::{info, warn};

#[cfg(unix)]
use crate::pane::Pane;

/// Top-level handoff blob serialized across the `execve`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handoff {
    /// Session id the new image must own (must match its `--session`).
    pub session: String,
    /// Engine name (`alacritty`).
    pub engine: String,
    /// Version of the image that produced this handoff (informational).
    pub from_version: String,
    /// Live panes to adopt.
    pub panes: Vec<PaneHandoff>,
}

/// Per-pane handoff entry. The PTY master is referenced by inherited fd
/// number; the child by pid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneHandoff {
    /// Pane id (e.g. `p1`).
    pub pane_id: String,
    /// Inherited PTY master fd number (CLOEXEC cleared before exec).
    pub master_fd: i32,
    /// Surviving child pid (`waitpid`-able — the daemon stayed its parent).
    pub child_pid: i32,
    /// Child process-group id, for group-aware teardown.
    pub pgid: Option<i32>,
    /// Geometry — columns.
    pub cols: u16,
    /// Geometry — rows.
    pub rows: u16,
    /// Remembered shell-style exit code if the pane was terminal-retained.
    pub last_exit: Option<i32>,
    /// Whether the pane had already terminated (its child is gone).
    pub terminal_retained: bool,
    /// Wall-clock spawn time (RFC3339), preserved so `list` stays stable.
    pub spawned_at: String,
    /// Write-lease holder uuid, if one was held.
    pub write_lease_holder: Option<String>,
    /// Path to the pane's `.cast` recording (the replay checkpoint).
    pub cast_path: Option<String>,
    /// Output ring high-water mark at handoff (cumulative bytes observed).
    pub ring_high_water: u64,
    /// argv the pane's child was launched with (for adapter re-detection).
    pub argv: Vec<String>,
}

/// Handle the `daemon upgrade` command: validate the new binary, snapshot the
/// live pane table into a [`Handoff`], ack the client, then (after the ack
/// flushes) `execve` into the new image in the SAME pid.
///
/// Returns an error `Response` — leaving the running daemon untouched — if the
/// new binary fails its smoke test. Re-exec is one-way, so we never exec a
/// binary we couldn't even run `--version` on.
pub async fn run(state: &DaemonState, binary: Option<String>) -> Response {
    // Resolve the binary to exec. Default: our own current executable (the
    // production path swaps the on-disk file first, then re-execs it).
    let binary_path = match binary {
        Some(b) => PathBuf::from(b),
        None => match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                return Response::err(ErrorBody::new(
                    ErrorCode::Internal,
                    format!("cannot resolve current executable: {e}"),
                    "pass --binary <path> explicitly",
                ));
            }
        },
    };

    // **Validate before the one-way door.** A broken new image would lose the
    // daemon, so smoke-test it (`--version` must exit 0) BEFORE we exec.
    if let Err(reason) = smoke_test(&binary_path) {
        return Response::err(ErrorBody::new(
            ErrorCode::Internal,
            format!("new binary failed smoke test: {reason}"),
            "the running daemon is untouched; fix the binary and retry",
        ));
    }

    // Snapshot the live pane table. U1 keeps the CHILD alive, so terminal-
    // retained panes (child already dead) are dropped — there is nothing to
    // preserve and signalling a stale pgid is hazardous.
    let panes = collect_panes(state).await;
    let handoff = Handoff {
        session: state.cfg.session.0.clone(),
        engine: state.cfg.engine.clone(),
        from_version: state.cfg.binary_version.clone(),
        panes,
    };

    let ack = serde_json::json!({
        "status": "upgrading",
        "pid": std::process::id(),
        "binary": binary_path.display().to_string(),
        "panes": handoff
            .panes
            .iter()
            .map(|p| serde_json::json!({ "pane": p.pane_id, "child_pid": p.child_pid }))
            .collect::<Vec<_>>(),
    });

    // Defer the exec so the ack above flushes to the client first (mirrors the
    // `daemon shutdown` pattern). The spawned task does the CLOEXEC clear +
    // fsync + execve; on success it never returns.
    let cfg = state.cfg.clone();
    let binary_for_exec = binary_path;
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        if let Err(e) = exec_into_new_image(&cfg, &binary_for_exec, &handoff) {
            // execve returned → it failed. The daemon keeps running (degraded:
            // its panes are intact, the upgrade just didn't happen). Loud log
            // so the operator notices; nothing is lost.
            error!(error = %e, "daemon upgrade re-exec failed; staying on current image");
        }
    });

    Response::ok(ack)
}

/// Smoke-test the candidate binary: `<binary> --version` must exit 0.
fn smoke_test(binary: &std::path::Path) -> Result<(), String> {
    let out = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|e| format!("spawn {} --version: {e}", binary.display()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} --version exited {:?}",
            binary.display(),
            out.status.code()
        ))
    }
}

/// Snapshot the live (non-terminal) panes into handoff entries.
#[cfg(unix)]
async fn collect_panes(state: &DaemonState) -> Vec<PaneHandoff> {
    let mut out = Vec::new();
    for pane in state.registry.all_panes().await {
        // U1: only carry live panes (child still running). Skip terminal-
        // retained ones.
        if pane.is_terminal() {
            continue;
        }
        let Some(master_fd) = pane.pty.raw_master_fd() else {
            warn!(pane = %pane.id, "no raw master fd; skipping pane in handoff");
            continue;
        };
        let Some(child_pid) = pane.pty.child_pid() else {
            warn!(pane = %pane.id, "no child pid; skipping pane in handoff");
            continue;
        };
        let (cols, rows) = pane.engine.dimensions();
        out.push(PaneHandoff {
            pane_id: pane.id.0.clone(),
            master_fd,
            #[allow(clippy::cast_possible_wrap)]
            child_pid: child_pid as i32,
            pgid: pane.pty.pgid(),
            cols,
            rows,
            last_exit: pane.remembered_exit(),
            terminal_retained: false,
            spawned_at: pane.spawned_at.to_rfc3339(),
            write_lease_holder: pane.lease_holder().map(|u| u.to_string()),
            cast_path: cast_path_for(&state.cfg.session.0, &pane.id)
                .map(|p| p.display().to_string()),
            ring_high_water: pane.pty.output_total(),
            argv: pane.argv.clone(),
        });
    }
    out
}

/// In-place upgrade is Unix-only (the Windows daemon process model is still
/// in design — see `run_daemon`). Hand off nothing.
#[cfg(not(unix))]
async fn collect_panes(_state: &DaemonState) -> Vec<PaneHandoff> {
    Vec::new()
}

/// `<state_root>/agent-tui/<session>/<pane>.cast` — same path the recorder
/// writes to (see `handlers::spawn::start_recorder`).
#[cfg(unix)]
fn cast_path_for(session: &str, pane: &PaneId) -> Option<PathBuf> {
    crate::paths::state_root().map(|d| {
        d.join("agent-tui")
            .join(session)
            .join(format!("{}.cast", pane.0))
    })
}

/// Clear `FD_CLOEXEC` on each pane's master fd, `fsync` each `.cast`, then
/// `execve` into `binary` with the handoff in [`HANDOFF_ENV`]. Only returns on
/// failure (`execve` replaces the process image on success).
#[cfg(unix)]
fn exec_into_new_image(
    cfg: &crate::DaemonConfig,
    binary: &std::path::Path,
    handoff: &Handoff,
) -> anyhow::Result<()> {
    use std::ffi::CString;

    // 1. fd lifecycle + cast durability for each pane.
    for p in &handoff.panes {
        clear_cloexec(p.master_fd)?;
        if let Some(cast) = &p.cast_path {
            fsync_path(cast);
        }
    }

    // 2. Serialize the blob.
    let blob = serde_json::to_string(handoff)?;

    // 3. Reconstruct the `daemon run` argv (globals BEFORE the subcommand). The
    // handoff rides as a hidden `--adopt-handoff` arg, NOT an env var, so it is
    // never inherited by the new daemon's PTY children.
    let exe = binary.to_string_lossy().into_owned();
    let mut argv: Vec<String> = vec![
        exe.clone(),
        "--session".into(),
        cfg.session.0.clone(),
        "--socket-dir".into(),
        cfg.layout.root.to_string_lossy().into_owned(),
        "--engine".into(),
        cfg.engine.clone(),
        "daemon".into(),
        "run".into(),
    ];
    if let Some(pid) = cfg.monitor_parent {
        argv.push("--monitor-parent".into());
        argv.push(pid.to_string());
    }
    if let Some(secs) = cfg.idle_timeout_secs {
        argv.push("--idle-timeout-secs".into());
        argv.push(secs.to_string());
    }
    argv.push("--adopt-handoff".into());
    argv.push(blob);

    // 4. Build envp: inherit current env, restating the allowlist so the new
    // image's governance matches (the CLI's `--allowed-binaries` reads it).
    let mut envp: Vec<CString> = Vec::new();
    for (k, v) in std::env::vars() {
        if k == "AGENT_TUI_ALLOWED_BINARIES" {
            continue;
        }
        envp.push(CString::new(format!("{k}={v}"))?);
    }
    if let Some(csv) = &cfg.allowed_binaries {
        envp.push(CString::new(format!("AGENT_TUI_ALLOWED_BINARIES={csv}"))?);
    }

    let c_path = CString::new(exe.as_bytes())?;
    let c_args: Vec<CString> = argv
        .iter()
        .map(|a| CString::new(a.as_bytes()))
        .collect::<Result<_, _>>()?;

    info!(
        binary = %binary.display(),
        panes = handoff.panes.len(),
        "re-exec'ing daemon into new image (same pid)"
    );

    // 5. The one-way door. On success this never returns.
    nix::unistd::execve(&c_path, &c_args, &envp)?;
    Ok(())
}

#[cfg(not(unix))]
fn exec_into_new_image(
    _cfg: &crate::DaemonConfig,
    _binary: &std::path::Path,
    _handoff: &Handoff,
) -> anyhow::Result<()> {
    anyhow::bail!("in-place upgrade is Unix-only")
}

/// Clear `FD_CLOEXEC` so `fd` survives the `execve` into the new image.
#[cfg(unix)]
fn clear_cloexec(fd: i32) -> anyhow::Result<()> {
    use std::os::fd::BorrowedFd;
    #[allow(unsafe_code)]
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let flags = rustix::io::fcntl_getfd(borrowed)?;
    rustix::io::fcntl_setfd(borrowed, flags.difference(rustix::io::FdFlags::CLOEXEC))?;
    Ok(())
}

/// `fsync` a file by path (best-effort). Forces the `.cast`'s consumed bytes
/// out of the page cache so the new image's replay sees them.
#[cfg(unix)]
fn fsync_path(path: &str) {
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.sync_all();
    }
}

/// Parse a handoff blob (the hidden `daemon run --adopt-handoff` arg).
#[must_use]
pub fn parse_handoff(blob: &str) -> Option<Handoff> {
    match serde_json::from_str::<Handoff>(blob) {
        Ok(h) => Some(h),
        Err(e) => {
            error!(error = %e, "ignoring malformed upgrade handoff blob");
            None
        }
    }
}

/// Adopt every pane in the handoff blob into `state.registry`: take the
/// inherited master fd, replay the `.cast` to rebuild the grid, resume the
/// reader loop, and restore last-exit / lease / geometry. Best-effort per pane
/// — a pane we can't adopt is logged and skipped rather than failing startup.
/// No-op when this image wasn't launched with a handoff (a fresh start).
#[cfg(unix)]
pub async fn adopt(state: &DaemonState) {
    let Some(blob) = state.cfg.adopt_handoff.as_deref() else {
        return;
    };
    let Some(handoff) = parse_handoff(blob) else {
        return;
    };
    info!(
        panes = handoff.panes.len(),
        from_version = %handoff.from_version,
        "adopting panes from in-place upgrade handoff"
    );
    for p in handoff.panes {
        if let Err(e) = adopt_one(state, &p).await {
            error!(pane = %p.pane_id, error = %e, "failed to adopt pane; skipping");
        }
    }
}

#[cfg(not(unix))]
pub async fn adopt(_state: &DaemonState) {}

#[cfg(unix)]
async fn adopt_one(state: &DaemonState, p: &PaneHandoff) -> anyhow::Result<()> {
    use chrono::DateTime;

    let pane_id = PaneId(p.pane_id.clone());

    // Rebuild the engine grid by replaying the `.cast` recording — the elegant
    // reuse: the recording IS the checkpoint. Replay the full current cast
    // file (rotated `.gz` segments are an L3/scale concern, deferred).
    let engine: Arc<dyn Engine> = Arc::new(AlacrittyEngine::new(p.cols, p.rows));
    if let Some(cast) = &p.cast_path {
        let replayed = replay_cast_into(&engine, cast);
        info!(pane = %p.pane_id, events = replayed, "replayed .cast to rebuild grid");
    }

    // Re-attach a recorder so post-upgrade output keeps appending to the same
    // cast (timing offset restarts at 0; replay ignores timestamps).
    let recorder = reopen_recorder(&state.cfg.session.0, &pane_id);
    if let Some(rec) = &recorder {
        crate::handlers::spawn::spawn_checkpoint_task(engine.clone(), rec.clone());
    }

    let pty = crate::pty::PtyChild::adopt(
        p.master_fd,
        p.child_pid,
        p.last_exit,
        engine.clone(),
        recorder,
    )?;

    // Restore geometry on the inherited terminal (defensive — the kernel
    // window size survives across the swap, but a resize that raced the
    // upgrade could have been lost).
    let _ = pty.resize(p.cols, p.rows);

    // Re-detect the adapter from argv (banner-based re-detection on live bytes
    // is an L2 refinement; argv detection matches the spawn-time first pass).
    let info = PaneInfo {
        argv: p.argv.clone(),
        comm: p
            .argv
            .first()
            .map(|a| a.rsplit(['/', '\\']).next().unwrap_or(a).to_string())
            .unwrap_or_default(),
        first_bytes: Vec::new(),
        env: std::collections::BTreeMap::new(),
    };
    let adapter = state
        .adapters
        .detect_best(&info)
        .await
        .unwrap_or_else(|| Arc::new(agent_tui_adapter::GenericAdapter));

    let spawned_at = DateTime::parse_from_rfc3339(&p.spawned_at)
        .map_or_else(|_| chrono::Utc::now(), |dt| dt.with_timezone(&chrono::Utc));
    let lease = p
        .write_lease_holder
        .as_ref()
        .and_then(|s| uuid::Uuid::parse_str(s).ok());

    let pane = Pane {
        id: pane_id.clone(),
        argv: p.argv.clone(),
        spawned_at,
        engine,
        pty,
        adapter: tokio::sync::RwLock::new(adapter),
        lease: std::sync::Mutex::new(lease),
        last_exit: std::sync::Mutex::new(p.last_exit),
    };

    let counter = p
        .pane_id
        .strip_prefix('p')
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0);
    state.registry.adopt_insert(pane, counter).await;
    info!(pane = %p.pane_id, child_pid = p.child_pid, "adopted pane after upgrade");
    Ok(())
}

/// Feed the `o` (output) events of a `.cast` file into `engine`, rebuilding
/// the grid. Returns the number of events replayed. Mirrors the CLI's
/// `replay_cast` reader (asciicast v2/v3, `[time, kind, payload]`).
#[cfg(unix)]
fn replay_cast_into(engine: &Arc<dyn Engine>, cast: &str) -> u64 {
    use std::io::{BufRead, BufReader};

    let Ok(file) = std::fs::File::open(cast) else {
        return 0;
    };
    let mut events = 0u64;
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() || !line.starts_with('[') {
            continue;
        }
        let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(line) else {
            continue;
        };
        if arr.get(1).and_then(serde_json::Value::as_str) != Some("o") {
            continue;
        }
        let Some(payload) = arr.get(2).and_then(serde_json::Value::as_str) else {
            continue;
        };
        if engine.feed(payload.as_bytes()).is_ok() {
            events += 1;
        }
    }
    events
}

/// Re-open a recorder appending to the pane's existing `.cast` (so post-
/// upgrade output continues the same recording). Mirrors
/// `handlers::spawn::start_recorder`.
#[cfg(unix)]
fn reopen_recorder(session: &str, pane: &PaneId) -> Option<Recorder> {
    let dir = crate::paths::state_root().map(|d| d.join("agent-tui").join(session))?;
    let cfg = RecorderConfig::new(dir, pane.0.clone());
    match Recorder::start(cfg) {
        Ok((rec, _handle)) => Some(rec),
        Err(e) => {
            warn!(error = %e, "recorder failed to re-open on adopt; continuing without one");
            None
        }
    }
}
