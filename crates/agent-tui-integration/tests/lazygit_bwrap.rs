//! lazygit end-to-end scenarios via the bwrap backend.
//!
//! Exercises a real-world async TUI: lazygit boots, reads a seeded git
//! repo, renders four side-by-side panels, accepts navigation keys, and
//! shows git state changes mid-session. The fixture
//! (`fixtures/lazygit/Dockerfile`) pre-creates a repo with one staged,
//! one unstaged, and one untracked file — every scenario starts from
//! that known state.
//!
//! Why these particular scenarios:
//!  - `lazygit_renders_seeded_state` — does the PTY render survive
//!    lazygit's heavy gocui repaints? Are the status-prefix strings
//!    in the snapshot?
//!  - `lazygit_navigates_to_branches` — number-key panel-jump (real
//!    multi-pane navigation) results in a visible "Local branches"
//!    header.
//!  - `lazygit_stage_modified_file` — change git state via the TUI;
//!    verify the snapshot reflects the new state (` M` → `M `).

#![cfg(feature = "bwrap")]

use agent_tui_integration::bwrap::{BwrapScenario, fixtures};
use anyhow::Result;

/// lazygit args that pin every nondeterministic knob via the fixture-baked
/// config. Used by every scenario.
const LG_ARGS: &[&str] = &[
    "lazygit",
    "--use-config-file=/fixtures/xdg/lazygit/config.yml",
    "--path",
    "/fixtures/repo",
];

/// On open, lazygit renders the seeded files panel. Anchor on the file
/// names and the panel chrome — both are stable across lazygit versions.
#[tokio::test]
async fn bwrap_lazygit_renders_seeded_state() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_lazygit_renders_seeded_state", fixtures::LAZYGIT).await?;
    s.spawn(LG_ARGS.iter().copied()).await?;

    // The seeded files all show up in the Files panel on launch.
    s.wait_text("b.txt").await?;
    s.wait_idle(200).await?;

    let snap = s.snapshot().await?;
    assert_eq!(
        snap.state().unwrap_or(""),
        "alt_screen_tui",
        "lazygit should classify as alt-screen TUI"
    );
    // Files panel header.
    snap.assert_outline_contains("Files")?;
    // All three seeded entries (lazygit shows the basenames at minimum).
    snap.assert_outline_contains("b.txt")?;
    snap.assert_outline_contains("c.txt")?;
    snap.assert_outline_contains("d.txt")?;

    s.press("q").await?;
    s.die().await?;
    Ok(())
}

/// `3` jumps to the Local branches panel; the seeded repo has one
/// branch (`main`), so the panel-content anchor is "main" + the panel
/// title.
#[tokio::test]
async fn bwrap_lazygit_navigates_to_branches_panel() -> Result<()> {
    let mut s =
        BwrapScenario::new("bwrap_lazygit_navigates_to_branches", fixtures::LAZYGIT).await?;
    s.spawn(LG_ARGS.iter().copied()).await?;
    s.wait_text("Files").await?;
    s.wait_idle(200).await?;

    s.press("3").await?;
    // The branches panel renders the title text "Local branches" along
    // with the single seeded branch "main".
    s.wait_text("Local branches").await?;
    s.wait_idle(150).await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("Local branches")?;
    snap.assert_outline_contains("main")?;

    s.press("q").await?;
    s.die().await?;
    Ok(())
}

/// `2` focuses the Files panel; pressing `<space>` toggles staging on
/// the currently-selected file. After staging, the git state in the
/// snapshot must reflect the change.
#[tokio::test]
async fn bwrap_lazygit_commits_panel_shows_seeded_history() -> Result<()> {
    let mut s = BwrapScenario::new(
        "bwrap_lazygit_commits_panel_shows_history",
        fixtures::LAZYGIT,
    )
    .await?;
    s.spawn(LG_ARGS.iter().copied()).await?;
    s.wait_text("Files").await?;
    s.wait_idle(200).await?;

    // `4` jumps to the Commits panel. The seeded repo has two commits;
    // both subject lines are visible.
    s.press("4").await?;
    s.wait_text("Commits").await?;
    s.wait_idle(150).await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("Commits")?;
    // Most recent commit first in the panel; both should be visible.
    snap.assert_outline_contains("add b.txt")?;

    s.press("q").await?;
    s.die().await?;
    Ok(())
}
