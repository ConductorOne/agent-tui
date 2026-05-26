//! Per-command handler implementations.
//!
//! Each handler is a free function that takes the daemon's shared state and
//! the command-specific arguments and returns a `Response`. `server.rs`
//! dispatches `Command` variants to these.

pub mod die;
pub mod input;
pub mod list;
pub mod raw;
pub mod signal;
pub mod snapshot;
pub mod spawn;
pub mod wait;
