//! Wire types for the agent-tui CLI ↔ daemon JSON-RPC line protocol.
//!
//! Every request and response is a single JSON object terminated by `\n`. The
//! daemon is the server; the CLI (and `mcp serve` mode) is the client. The
//! schemas here are the source of truth for both ends.
//!
//! See `docs/design/RFC.md` §5.3 (envelope) and §5.4 (error codes).

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

pub mod action;
pub mod error;
pub mod keymap;
pub mod request;
pub mod response;
pub mod selector;
pub mod snapshot;

pub use action::{ActionDetail, ActionKind, AuditEvent, Decision, Verdict};
pub use error::{ErrorCode, ProtocolError};
pub use request::{Command, Request};
pub use response::{ErrorBody, Response, ResponseEnvelope, ToolOutputDelim, Warning};
pub use selector::{
    ParseError as SelectorParseError, Selector, all_refs as outline_all_refs,
    format_parse_error as format_selector_parse_error,
};
pub use snapshot::{
    Cell, CellGrid, CursorInfo, ModeFlags, Outline, OutlineNode, PaneState, PngInfo, Ref,
    RefBinding, Snapshot,
};

/// Current wire-protocol version. Bumped on incompatible changes.
///
/// Major drift between CLI and daemon triggers `DAEMON_VERSION_DRIFT`
/// (or `DAEMON_VERSION_DRIFT_ACTIVE` if any pane is in a non-shell state).
pub const PROTOCOL_VERSION: u32 = 1;

/// Pane identifier — monotonic per session, never reused.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaneId(pub String);

impl PaneId {
    /// Construct a pane id from a numeric counter, formatted as `p<N>`.
    #[must_use]
    pub fn from_counter(n: u64) -> Self {
        Self(format!("p{n}"))
    }
}

impl std::fmt::Display for PaneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Session identifier — typically the value of `--session` (default
/// `"default"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

impl SessionId {
    /// The default session name used when `--session` is not specified.
    #[must_use]
    pub fn default_name() -> Self {
        Self("default".to_string())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::default_name()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Monotonic mutation-sequence number for a pane. Incremented by 1 on every
/// engine mutation. Stable for the life of the pane.
pub type Sequence = u64;

/// Monotonic generation number for a pane. Incremented only when a snapshot
/// observes a sequence higher than the last-snapshotted sequence.
pub type Generation = u64;
