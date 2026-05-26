//! `agent-tui` — entry point.
//!
//! Parses the CLI surface defined in `docs/RFC.md` §5, then either spawns /
//! talks to the per-session daemon (most subcommands) or hosts the daemon
//! itself (`daemon` subcommand).

#![forbid(unsafe_code)]
// The binary has no external API surface — every `pub` item is for
// intra-crate ergonomics, so the workspace's `unreachable_pub` warning
// doesn't carry weight here.
#![allow(unreachable_pub)]

mod cli;
mod client;
mod commands;
mod mcp;

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    install_tracing();
    let cli = cli::Cli::parse();
    commands::dispatch(cli).await
}

fn install_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}
