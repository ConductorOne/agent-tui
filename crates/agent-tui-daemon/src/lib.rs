//! Long-lived per-session daemon for `agent-tui`.
//!
//! Speaks JSON-RPC over a Unix-domain socket — one line per request,
//! one line per response. The CLI lazily spawns the daemon if no live socket
//! is found at `$XDG_RUNTIME_DIR/agent-tui/<session>.sock`.
//!
//! Owns the per-session resources every handler needs: pane registry, the
//! seq→hash window backing `wait --hash`, snapshot generation tracker, and
//! the adapter registry consulted at spawn time. See `docs/design/RFC.md` §2, §4,
//! §5, §13.1.

// Crate-level `deny` rather than `forbid` so a few Windows-only FFI paths can
// opt into `unsafe` via `#[allow(unsafe_code)]`: the parent-process monitor
// (server.rs), the descendant-tree kill (pty.rs), and the detached daemon spawn
// with an explicit handle-inheritance allow-list (win_spawn.rs). Every other
// module stays unsafe-free.
#![deny(unsafe_code)]

pub mod adapter_registry;
pub mod classifier;
pub mod governance;
pub mod handlers;
pub mod hash_window;
pub mod osc133;
pub mod pane;
pub mod paths;
pub mod pty;
pub mod render;
pub mod server;
pub mod sidecar;
pub mod upgrade;
#[cfg(windows)]
pub mod win_spawn;

pub use paths::SocketLayout;
pub use server::{DaemonConfig, DaemonHandle, run_daemon};
