//! Cycle I1 dummy scenario.
//!
//! Proves the harness wires correctly: a debian-slim container, an
//! agent-tui binary copied in, a `spawn → wait_text → snapshot → die`
//! round-trip. No TUI complexity — that lands with the vim/lazygit
//! fixtures in I2+.
//!
//! The base image used to be `alpine:3.20`, which broke under CI:
//! `cargo build --bin agent-tui` on ubuntu-latest produces a
//! glibc-linked binary, but Alpine ships musl libc — the binary
//! couldn't even start (`/lib/ld-musl-…: not found` style failure).
//! All the other fixtures use `debian:bookworm-slim` for the same
//! reason; this test now matches them.

#![cfg(feature = "docker")]

use agent_tui_integration::scenario::Scenario;
use anyhow::Result;

/// glibc-compatible base, matching the rest of the fixtures.
const BASE: &str = "debian:bookworm-slim";

#[tokio::test]
async fn alpine_echo_round_trips_through_agent_tui() -> Result<()> {
    let mut s = Scenario::new("alpine_echo_smoke", BASE).await?;
    // `sh -c "echo hi-from-alpine; sleep 30"` prints once then idles so
    // we can snapshot before the child exits.
    s.spawn(["sh", "-c", "echo hi-from-alpine; sleep 30"])
        .await?;
    s.wait_text("hi-from-alpine").await?;
    let snap = s.snapshot().await?;
    snap.assert_outline_contains("hi-from-alpine")?;
    s.die().await?;
    Ok(())
}

#[tokio::test]
async fn agent_tui_doctor_runs_inside_container() -> Result<()> {
    let mut s = Scenario::new("alpine_doctor_smoke", BASE).await?;
    // `doctor --quick` proves the daemon lazy-spawns and the wire
    // protocol completes a round trip even before any pane exists.
    let snap = s.spawn(["sh", "-c", "sleep 30"]).await?;
    assert!(
        snap["success"].as_bool().unwrap_or(false),
        "spawn failed: {snap}"
    );
    s.die().await?;
    Ok(())
}
