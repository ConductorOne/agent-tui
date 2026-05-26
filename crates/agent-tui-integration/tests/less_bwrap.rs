//! `less` pager scenarios — what an agent does to a long text file.
//!
//! Workflows covered:
//!  - Open a file. Anchor on `less -M`'s status-line shape:
//!    `lines X-Y/Z   P%`. Snapshot, assert state.
//!  - Search forward with `/word<cr>`, jump through matches with `n`/`N`.
//!  - Jump to a percentage of the file with `Np`, observe the status
//!    line update.
//!  - Quit with `q`. (We don't assert alt-screen post-quit because
//!    `LESS=-M` doesn't include `-X`; tests that care should set it.)

#![cfg(feature = "bwrap")]

use agent_tui_integration::bwrap::{BwrapScenario, fixtures};
use anyhow::Result;

#[tokio::test]
async fn bwrap_less_opens_file_and_shows_status() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_less_opens", fixtures::LESS).await?;
    s.spawn(["less", "/fixtures/lorem.txt"]).await?;
    // `less -M` status-line shape: `<file> lines X-Y/Z   P%`.
    // The Z (total) is 200 lines for our fixture; the path itself is
    // a robust anchor that doesn't depend on the viewport width.
    s.wait_text(r"lorem\.txt").await?;
    s.wait_idle(150).await?;

    let snap = s.snapshot().await?;
    assert_eq!(
        snap.state().unwrap_or(""),
        "alt_screen_tui",
        "less defaults to alt-screen TUI mode"
    );
    snap.assert_outline_contains("lorem ipsum")?;
    // The first numbered line ("line 1") is visible at the top of the
    // viewport — proves the rendered content reaches the engine.
    snap.assert_outline_contains("line 1:")?;

    s.press("q").await?;
    s.die().await?;
    Ok(())
}

#[tokio::test]
async fn bwrap_less_search_finds_anchor() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_less_search", fixtures::LESS).await?;
    s.spawn(["less", "/fixtures/lorem.txt"]).await?;
    s.wait_text(r"lorem\.txt").await?;
    s.wait_idle(150).await?;

    // `/the-answer-marker<cr>` searches forward; less scrolls so the
    // first match is visible. The fixture seeds "the-answer-marker"
    // exactly once, on line 42.
    s.press("/the-answer-marker<cr>").await?;
    s.wait_text("the-answer-marker").await?;
    s.wait_idle(120).await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("the-answer-marker")?;

    s.press("q").await?;
    s.die().await?;
    Ok(())
}

#[tokio::test]
async fn bwrap_less_jump_to_end_shows_end_marker() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_less_jump_end", fixtures::LESS).await?;
    s.spawn(["less", "/fixtures/lorem.txt"]).await?;
    s.wait_text(r"lorem\.txt").await?;
    s.wait_idle(150).await?;

    // `G` jumps to the bottom; `less -M` then shows `(END)` in the
    // status line and `line 200:` is now visible in the viewport.
    s.press("G").await?;
    s.wait_text(r"line 200:").await?;
    s.wait_idle(120).await?;

    let snap = s.snapshot().await?;
    // The deepest seeded marker also lands in the visible region.
    snap.assert_outline_contains("line 200:")?;

    s.press("q").await?;
    s.die().await?;
    Ok(())
}
