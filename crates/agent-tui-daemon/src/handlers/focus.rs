//! `pane focus` command handler.
//!
//! Pin the registry's focused pane so subsequent no-`--pane` commands
//! resolve to it. Passing `pane: None` clears the focus.

use std::sync::Arc;

use agent_tui_protocol::{ErrorBody, ErrorCode, PaneId, Response};

use crate::pane::Registry;

/// Set or clear the focused pane.
pub async fn run(registry: &Arc<Registry>, pane: Option<PaneId>) -> Response {
    let target = pane.clone();
    if registry.set_focused(target).await {
        Response::ok(serde_json::json!({
            "focused": pane,
        }))
    } else {
        Response::err(ErrorBody::new(
            ErrorCode::NoActivePane,
            format!(
                "pane {} not found",
                pane.as_ref()
                    .map_or_else(|| "<unknown>".into(), |p| p.0.clone())
            ),
            "call list to see live panes",
        ))
    }
}
