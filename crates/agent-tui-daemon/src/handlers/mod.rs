//! Per-command handler implementations.
//!
//! Each handler is a free function that takes the daemon's shared state and
//! the command-specific arguments and returns a `Response`. `server.rs`
//! dispatches `Command` variants to these.

pub mod die;
pub mod list;
pub mod snapshot;
pub mod spawn;
