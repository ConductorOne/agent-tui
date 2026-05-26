//! `die` command handler.
//!
//! Sends SIGTERM via the killer handle, then drops the registry entry. The
//! reader task aborts on drop; subsequent `list` calls no longer report the
//! pane.

use std::sync::Arc;

use agent_tui_protocol::{ErrorBody, ErrorCode, PaneId, Response};

use crate::pane::Registry;

/// Resolve the pane id (or focused-pane heuristic), kill the child, and drop
/// the registry entry.
pub async fn run(registry: &Arc<Registry>, pane: Option<PaneId>) -> Response {
    let id = match resolve(registry, pane).await {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    let Some(entry) = registry.remove(&id).await else {
        return Response::err(ErrorBody::new(
            ErrorCode::NoActivePane,
            format!("pane {id} not found"),
            "call list to see live panes",
        ));
    };

    // Best-effort SIGTERM. If the child has already exited, ChildKiller
    // typically returns an error; treat it as success.
    let kill_err = entry.pty.kill().err().map(|e| e.to_string());

    Response::ok(serde_json::json!({
        "pane": id,
        "killed": kill_err.is_none(),
        "kill_error": kill_err,
    }))
}

async fn resolve(registry: &Arc<Registry>, pane: Option<PaneId>) -> Result<PaneId, Response> {
    if let Some(id) = pane {
        return Ok(id);
    }
    // Focused-pane semantics arrive in P0b; for now require explicit id when
    // more than one pane exists.
    let list = registry.list().await;
    match list.len() {
        1 => Ok(list[0].id.clone()),
        0 => Err(Response::err(ErrorBody::new(
            ErrorCode::NoActivePane,
            "no panes",
            "spawn a pane first",
        ))),
        _ => Err(Response::err(ErrorBody::new(
            ErrorCode::NoActivePane,
            "multiple panes; --pane required",
            "pass --pane p<N> (focus tracking lands in P0b)",
        ))),
    }
}
