//! `spawn` command handler.
//!
//! Allocates a fresh `PaneId`, instantiates an engine, spawns a PTY child,
//! and registers the pane.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent_tui_adapter::PaneInfo;
use agent_tui_engine::Engine;
use agent_tui_engine_alacritty::AlacrittyEngine;
use agent_tui_protocol::{ErrorBody, ErrorCode, Response, SessionId};
use agent_tui_recorder::{Recorder, RecorderConfig};
use chrono::Utc;

use crate::adapter_registry::AdapterRegistry;
use crate::governance::{Governance, build};
use crate::pane::{Pane, Registry};
use crate::pty::PtyChild;

const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;

/// Spawn a PTY-backed pane running `argv` under the active session.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    session: &SessionId,
    registry: &Arc<Registry>,
    adapters: &AdapterRegistry,
    governance: &Governance,
    argv: Vec<String>,
    cwd: Option<String>,
    size: Option<(u16, u16)>,
    stdin_mode: agent_tui_protocol::request::StdinMode,
    env: Vec<(String, String)>,
) -> Response {
    if argv.is_empty() {
        return Response::err(ErrorBody::new(
            ErrorCode::InvalidArgs,
            "spawn requires a non-empty argv",
            "pass at least one positional argument",
        ));
    }

    // Governance gate — typed Spawn action through the configured Evaluator.
    let decision = governance
        .check(build::spawn(argv.clone(), cwd.clone().unwrap_or_default()))
        .await;
    if let Some(resp) = policy_response(&decision) {
        return resp;
    }
    let argv = canonicalize_absolute_argv0(argv);

    let (cols, rows) = size.unwrap_or((DEFAULT_COLS, DEFAULT_ROWS));
    let engine: Arc<dyn Engine> = Arc::new(AlacrittyEngine::new(cols, rows));
    let cwd_path = cwd.as_ref().map(PathBuf::from);

    let id = registry.alloc_id();
    let recorder = start_recorder(session, &id);
    if let Some(rec) = &recorder {
        spawn_checkpoint_task(engine.clone(), rec.clone());
    }

    let pty = match PtyChild::spawn(
        &argv,
        cwd_path.as_deref(),
        cols,
        rows,
        engine.clone(),
        recorder,
        stdin_mode,
        &env,
    ) {
        Ok(p) => p,
        Err(e) => {
            // A failed spawn is almost always caller-fixable (bad argv,
            // binary not on PATH, not executable) rather than an
            // agent-tui bug — surface it as SPAWN_FAILED, not INTERNAL.
            return Response::err(ErrorBody::new(
                ErrorCode::SpawnFailed,
                format!("pty spawn failed: {e}"),
                format!("verify `{}` exists, is executable, and is on PATH", argv[0]),
            ));
        }
    };

    let pane_info = PaneInfo {
        argv: argv.clone(),
        comm: basename(&argv[0]),
        first_bytes: Vec::new(),
        env: BTreeMap::new(),
    };
    let adapter = adapters
        .detect_best(&pane_info)
        .await
        .unwrap_or_else(|| Arc::new(agent_tui_adapter::GenericAdapter));
    let adapter_name = adapter.name().to_string();

    let pane = Pane {
        id: id.clone(),
        argv: argv.clone(),
        spawned_at: Utc::now(),
        engine,
        pty,
        adapter: tokio::sync::RwLock::new(adapter),
        lease: std::sync::Mutex::new(None),
        last_exit: std::sync::Mutex::new(None),
    };
    let pane_arc = registry.insert(pane).await;
    spawn_redetect_task(pane_arc, adapters.clone(), argv.clone());

    Response::ok(serde_json::json!({
        "pane": id,
        "argv": argv,
        "cols": cols,
        "rows": rows,
        "adapter": adapter_name,
    }))
}

/// Basename for `PaneInfo.comm`. Strips both Unix and Windows path
/// separators and the Windows `.exe` suffix (case-insensitive) so adapter
/// detection tables don't need to carry per-OS spellings.
fn basename(path: &str) -> String {
    let stem = path.rsplit(['/', '\\']).next().unwrap_or(path);
    // Strip a trailing `.exe` (case-insensitive) so e.g. `claude.exe` ->
    // `claude` and provider manifests recognize it.
    if stem.len() > 4 && stem[stem.len() - 4..].eq_ignore_ascii_case(".exe") {
        return stem[..stem.len() - 4].to_string();
    }
    stem.to_string()
}

fn canonicalize_absolute_argv0(mut argv: Vec<String>) -> Vec<String> {
    if let Some(first) = argv.first_mut() {
        let path = Path::new(first);
        if path.is_absolute() {
            if let Ok(canonical) = path.canonicalize() {
                *first = canonical.to_string_lossy().into_owned();
            }
        }
    }
    argv
}

/// Translate a non-Allow `Decision` into a `POLICY_*` Response. Allow returns
/// `None`, signalling the handler should proceed.
fn policy_response(decision: &agent_tui_protocol::Decision) -> Option<Response> {
    use agent_tui_protocol::Verdict;
    match decision.verdict {
        Verdict::Allow => None,
        Verdict::Deny => Some(Response::err(ErrorBody::new(
            ErrorCode::PolicyDenied,
            decision.reason.clone(),
            "spawn blocked; loosen --allowed-binaries or pick a different argv",
        ))),
        Verdict::RequireConfirm => Some(Response::err(ErrorBody::new(
            ErrorCode::PolicyPending,
            decision.reason.clone(),
            "human confirmation required; `policy confirm <id>` lands in a follow-on cycle",
        ))),
    }
}

/// Open a recorder under `$XDG_STATE_HOME/agent-tui/<session>/`. Returns
/// `None` (and logs) if we can't pick a state dir.
fn start_recorder(session: &SessionId, pane: &agent_tui_protocol::PaneId) -> Option<Recorder> {
    let dir = crate::paths::state_root().map(|d| d.join("agent-tui").join(&session.0))?;
    let cfg = RecorderConfig::new(dir, pane.0.clone());
    match Recorder::start(cfg) {
        Ok((rec, _handle)) => Some(rec),
        Err(e) => {
            tracing::warn!(error = %e, "recorder failed to start; continuing without one");
            None
        }
    }
}

/// Spawn a deferred adapter-redetect task. Waits briefly for the child to
/// produce its first ~512 bytes, then re-runs Detect with the populated
/// `PaneInfo.first_bytes` so banner-based adapters (claude-code, eventually
/// nvim/tmux) can upgrade `generic`.
const REDETECT_BUDGET_MS: u64 = 600;

fn spawn_redetect_task(
    pane: Arc<crate::pane::Pane>,
    adapters: crate::adapter_registry::AdapterRegistry,
    argv: Vec<String>,
) {
    tokio::spawn(async move {
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(REDETECT_BUDGET_MS);
        while !pane.pty.first_bytes_done() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        let first_bytes = pane.pty.first_bytes_snapshot();
        if first_bytes.is_empty() {
            return;
        }
        let info = PaneInfo {
            argv: argv.clone(),
            comm: basename(&argv[0]),
            first_bytes,
            env: BTreeMap::new(),
        };
        if let Some(best) = adapters.detect_best(&info).await {
            let current = pane.adapter().await;
            if best.name() != current.name() {
                let _ = pane.set_adapter(best.clone()).await;
                tracing::info!(
                    pane = %pane.id,
                    new_adapter = %best.name(),
                    previous = %current.name(),
                    "adapter upgraded after first-bytes re-detect"
                );
            }
        }
    });
}

/// Spawn a per-pane background task that emits an `s` checkpoint event into
/// the recorder every `CHECKPOINT_EVERY` mutations. Drops itself when the
/// engine's broadcast channel closes (i.e. the pane is destroyed).
const CHECKPOINT_EVERY: u64 = 1000;

pub(crate) fn spawn_checkpoint_task(engine: Arc<dyn Engine>, recorder: Recorder) {
    tokio::spawn(async move {
        let mut sub = engine.subscribe();
        let mut last_checkpointed: u64 = 0;
        while let Ok(evt) = sub.recv().await {
            if evt.sequence.saturating_sub(last_checkpointed) >= CHECKPOINT_EVERY {
                let snap = engine.snapshot();
                recorder.push_checkpoint(snap.sequence, &snap.canonical_hash());
                last_checkpointed = snap.sequence;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::basename;

    #[test]
    fn basename_strips_unix_path() {
        assert_eq!(basename("/usr/bin/bash"), "bash");
    }

    #[test]
    fn basename_strips_windows_path_and_exe() {
        assert_eq!(basename("C:\\Windows\\System32\\cmd.exe"), "cmd");
        assert_eq!(basename("CLAUDE.EXE"), "CLAUDE");
        assert_eq!(basename("claude.Exe"), "claude");
    }

    #[test]
    fn basename_passthrough_for_simple_name() {
        assert_eq!(basename("bash"), "bash");
    }

    #[test]
    fn basename_does_not_strip_non_exe_extension() {
        assert_eq!(basename("script.sh"), "script.sh");
    }
}
