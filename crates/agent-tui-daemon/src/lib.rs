//! Long-lived per-session daemon for `agent-tui`.
//!
//! Speaks JSON-RPC over a Unix-domain socket — one line per request,
//! one line per response. The CLI lazily spawns the daemon if no live socket
//! is found at `$XDG_RUNTIME_DIR/agent-tui/<session>.sock`.
//!
//! This crate is intentionally thin in v0.1.0 — it stands up the socket
//! layout, version handshake, and command dispatch surface so that the
//! per-pane queue, engine, recorder, and adapter wiring (P0–P1) can land
//! independently.
//!
//! See `docs/RFC.md` §2, §4, §5, §13.1.

#![forbid(unsafe_code)]

pub mod adapter_registry;
pub mod classifier;
pub mod handlers;
pub mod hash_window;
pub mod pane;
pub mod paths;
pub mod pty;
pub mod server;
pub mod sidecar;

pub use paths::SocketLayout;
pub use server::{DaemonConfig, DaemonHandle, run_daemon};
