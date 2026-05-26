//! `spawn` command handler.
//!
//! Allocates a fresh `PaneId`, instantiates an engine, spawns a PTY child,
//! and registers the pane.

use std::path::PathBuf;
use std::sync::Arc;

use agent_tui_engine::Engine;
use agent_tui_engine_alacritty::AlacrittyEngine;
use agent_tui_protocol::{ErrorBody, ErrorCode, Response, SessionId};
use agent_tui_recorder::{Recorder, RecorderConfig};
use chrono::Utc;

use crate::pane::{Pane, Registry};
use crate::pty::PtyChild;

const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;

/// Spawn a PTY-backed pane running `argv` under the active session.
pub async fn run(
    session: &SessionId,
    registry: &Arc<Registry>,
    argv: Vec<String>,
    cwd: Option<String>,
    size: Option<(u16, u16)>,
) -> Response {
    if argv.is_empty() {
        return Response::err(ErrorBody::new(
            ErrorCode::InvalidArgs,
            "spawn requires a non-empty argv",
            "pass at least one positional argument",
        ));
    }

    let (cols, rows) = size.unwrap_or((DEFAULT_COLS, DEFAULT_ROWS));
    let engine: Arc<dyn Engine> = Arc::new(AlacrittyEngine::new(cols, rows));
    let cwd_path = cwd.as_ref().map(PathBuf::from);

    let id = registry.alloc_id();
    let recorder = start_recorder(session, &id);

    let pty = match PtyChild::spawn(
        &argv,
        cwd_path.as_deref(),
        cols,
        rows,
        engine.clone(),
        recorder,
    ) {
        Ok(p) => p,
        Err(e) => {
            return Response::err(ErrorBody::new(
                ErrorCode::Internal,
                format!("pty spawn failed: {e}"),
                "verify the binary exists and is on PATH",
            ));
        }
    };
    let pane = Pane {
        id: id.clone(),
        argv: argv.clone(),
        spawned_at: Utc::now(),
        cols,
        rows,
        engine,
        pty,
    };
    registry.insert(pane).await;

    Response::ok(serde_json::json!({
        "pane": id,
        "argv": argv,
        "cols": cols,
        "rows": rows,
    }))
}

/// Open a recorder under `$XDG_STATE_HOME/agent-tui/<session>/`. Returns
/// `None` (and logs) if we can't pick a state dir.
fn start_recorder(session: &SessionId, pane: &agent_tui_protocol::PaneId) -> Option<Recorder> {
    let dir = state_dir().map(|d| d.join("agent-tui").join(&session.0))?;
    let cfg = RecorderConfig::new(dir, pane.0.clone());
    match Recorder::start(cfg) {
        Ok((rec, _handle)) => Some(rec),
        Err(e) => {
            tracing::warn!(error = %e, "recorder failed to start; continuing without one");
            None
        }
    }
}

fn state_dir() -> Option<PathBuf> {
    if let Ok(s) = std::env::var("XDG_STATE_HOME") {
        return Some(PathBuf::from(s));
    }
    dirs::state_dir()
}
