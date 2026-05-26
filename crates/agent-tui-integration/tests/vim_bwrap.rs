//! bwrap-backend port of the vim scenarios.
//!
//! Same assertions as `vim_basic.rs`, different runtime: instead of
//! `testcontainers` orchestrating a Docker container, the agent-tui
//! daemon runs on the host and spawns `bwrap ... -- vim ...` as the
//! PTY child. Hermeticity comes from the same OCI rootfs the Docker
//! backend uses (extracted from the same Dockerfile via
//! `just rootfs vim`).
//!
//! Gated on the `bwrap` feature so `cargo test --workspace` still
//! skips these. Locally: `just rootfs vim && just test-bwrap`.

#![cfg(feature = "bwrap")]

use agent_tui_integration::bwrap::{BwrapScenario, fixtures};
use anyhow::Result;

#[tokio::test]
async fn bwrap_vim_opens_file_and_shows_content() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_vim_opens_file", fixtures::VIM).await?;
    s.spawn(["vim", "/fixtures/sample.txt"]).await?;
    s.wait_text(r"sample\.txt").await?;
    s.wait_idle(120).await?;

    let snap = s.snapshot().await?;
    let state = snap.state().unwrap_or("");
    assert_eq!(
        state, "alt_screen_tui",
        "vim should classify as alt-screen TUI; got {state:?}"
    );

    snap.assert_outline_contains("first line")?;

    s.press(":q!<cr>").await?;
    s.die().await?;
    Ok(())
}

#[tokio::test]
async fn bwrap_vim_edit_save_round_trip() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_vim_edit_save", fixtures::VIM).await?;
    // Edit through /work so the writable mount gets exercised, not the
    // read-only rootfs.
    let src = "/fixtures/sample.txt";
    let dst = "/work/sample.txt";
    s.spawn(["bash", "-c", &format!("cp {src} {dst}; vim {dst}")])
        .await?;
    s.wait_text(r"sample\.txt").await?;
    s.wait_idle(120).await?;

    s.press("i hello-from-bwrap<esc>").await?;
    s.wait_text(r"\[\+\]").await?;

    s.press(":w<cr>").await?;
    s.wait_text(r"written").await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("hello-from-bwrap")?;

    s.press(":q!<cr>").await?;
    s.die().await?;
    Ok(())
}

#[tokio::test]
async fn bwrap_vim_search_finds_target() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_vim_search", fixtures::VIM).await?;
    s.spawn(["vim", "/fixtures/search-target.txt"]).await?;
    s.wait_text(r"search-target\.txt").await?;
    s.wait_idle(120).await?;

    s.press("/foo two<cr>").await?;
    s.wait_idle(120).await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("foo two")?;

    s.press(":q!<cr>").await?;
    s.die().await?;
    Ok(())
}
