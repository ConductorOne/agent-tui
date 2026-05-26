//! `spawn` command handler.
//!
//! Allocates a fresh `PaneId`, instantiates an engine, spawns a PTY child,
//! and registers the pane.

use std::path::PathBuf;
use std::sync::Arc;

use agent_tui_engine::Engine;
use agent_tui_engine_alacritty::AlacrittyEngine;
use agent_tui_protocol::{ErrorBody, ErrorCode, Response};
use chrono::Utc;

use crate::pane::{Pane, Registry};
use crate::pty::PtyChild;

const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;

/// Spawn a PTY-backed pane running `argv` under the active session.
pub async fn run(
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

    let pty = match PtyChild::spawn(&argv, cwd_path.as_deref(), cols, rows, engine.clone()) {
        Ok(p) => p,
        Err(e) => {
            return Response::err(ErrorBody::new(
                ErrorCode::Internal,
                format!("pty spawn failed: {e}"),
                "verify the binary exists and is on PATH",
            ));
        }
    };

    let id = registry.alloc_id();
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
