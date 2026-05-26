//! `list` command handler.

use std::sync::Arc;

use agent_tui_protocol::Response;

use crate::pane::Registry;

/// Enumerate live panes in the session.
pub async fn run(registry: &Arc<Registry>, _all: bool) -> Response {
    // `--all` would cross sessions; v0.1.0 has one session per daemon, so
    // we always return the local registry.
    let panes = registry.list().await;
    Response::ok(serde_json::json!({ "panes": panes }))
}
