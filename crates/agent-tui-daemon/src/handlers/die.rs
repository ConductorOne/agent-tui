//! `die` command handler.
//!
//! Sends SIGTERM via the killer handle, then drops the registry entry. The
//! reader task aborts on drop; subsequent `list` calls no longer report the
//! pane.

use std::sync::Arc;

use agent_tui_protocol::{ErrorBody, ErrorCode, PaneId, Response};

use crate::pane::{Registry, resolve_focused};

/// Resolve the target pane, kill the child, and drop the registry entry.
/// Clears focus if the killed pane was the focused one.
pub async fn run(registry: &Arc<Registry>, pane: Option<PaneId>) -> Response {
    let id = match resolve_focused(registry, pane).await {
        Ok(p) => p.id.clone(),
        Err(resp) => return resp,
    };

    let Some(entry) = registry.remove(&id).await else {
        return Response::err(ErrorBody::new(
            ErrorCode::NoActivePane,
            format!("pane {id} not found"),
            "call list to see live panes",
        ));
    };

    // Demote focus to `Held` when the focused pane dies. Future no-`--pane`
    // commands error until the user re-focuses explicitly.
    registry.mark_focus_held_if(&id).await;

    // Best-effort SIGTERM. If the child has already exited, ChildKiller
    // typically returns an error; treat it as success.
    let kill_err = entry.pty.kill().err().map(|e| e.to_string());

    Response::ok(serde_json::json!({
        "pane": id,
        "killed": kill_err.is_none(),
        "kill_error": kill_err,
    }))
}
