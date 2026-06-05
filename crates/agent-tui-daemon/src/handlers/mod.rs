//! Per-command handler implementations.
//!
//! Each handler is a free function that takes the daemon's shared state and
//! the command-specific arguments and returns a `Response`. `server.rs`
//! dispatches `Command` variants to these.

pub mod die;
pub mod focus;
pub mod input;
pub mod list;
pub mod raw;
pub mod signal;
pub mod snapshot;
pub mod spawn;
pub mod wait;

use agent_tui_protocol::{ErrorBody, ErrorCode, Response};

use crate::pane::Pane;

/// Gate a write (`type`/`press`/`send-ansi`/`stdin`) on the pane's write-lease.
/// Returns `Some(error)` when a lease is held by someone other than `lease`
/// (EBUSY-style), or `None` when the write is permitted (no lease, or the
/// caller is the holder). Free-for-all when no lease is outstanding.
pub(crate) fn lease_denied(pane: &Pane, lease: Option<uuid::Uuid>) -> Option<Response> {
    if pane.write_allowed(lease) {
        return None;
    }
    let holder = pane.lease_holder();
    Some(Response::err(ErrorBody::new(
        ErrorCode::InvalidArgs,
        match holder {
            Some(h) => format!("pane write-lease is held by {h}; this writer lacks the token"),
            None => "pane write is leased to another client".to_string(),
        },
        "attach with --write-lease to acquire the token, or wait for the holder to disconnect",
    )))
}
