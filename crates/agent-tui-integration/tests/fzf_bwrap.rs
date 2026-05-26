//! fzf (fuzzy picker) scenarios.
//!
//! fzf is unique in our matrix because it's *interactive but exits on
//! select* — the user types to narrow candidates, then presses Enter
//! and the program is gone. Test pattern: snapshot WHILE fzf is alive
//! (between keystrokes), then exit via Enter or Ctrl-C.
//!
//! All scenarios use `--layout=reverse` so the prompt is on the top
//! row — easier to anchor than the default bottom-up layout where
//! the counter and prompt move with the candidate count.

#![cfg(feature = "bwrap")]

use agent_tui_integration::bwrap::{BwrapScenario, fixtures};
use anyhow::Result;

#[tokio::test]
async fn bwrap_fzf_opens_with_candidate_list() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_fzf_opens", fixtures::FZF).await?;
    // Pipe the seeded fruit list as stdin to fzf. The bash wrapper is
    // necessary so the shell handles the pipe — fzf itself doesn't
    // take a `--input-file` flag in 0.38.
    s.spawn([
        "bash",
        "-c",
        "cat /fixtures/fruits.txt | fzf --layout=reverse --no-mouse --height=100%",
    ])
    .await?;
    // The prompt `> ` is on the top row in reverse layout. The
    // candidate count `  10/10` is on the second row.
    s.wait_text(r"10/10").await?;
    s.wait_idle(150).await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("apple")?;
    snap.assert_outline_contains("banana")?;
    snap.assert_outline_contains("10/10")?;

    // Ctrl-C exits with code 130 — the bash wrapper sees that and
    // doesn't echo anything. Just clean up.
    s.press("<c-c>").await?;
    s.die().await?;
    Ok(())
}

#[tokio::test]
async fn bwrap_fzf_typed_filter_narrows_candidates() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_fzf_filter", fixtures::FZF).await?;
    s.spawn([
        "bash",
        "-c",
        "cat /fixtures/fruits.txt | fzf --layout=reverse --no-mouse --height=100%",
    ])
    .await?;
    s.wait_text(r"10/10").await?;
    s.wait_idle(150).await?;

    // Type `ban` — fuzzy match. fzf updates the counter to show only
    // matching rows. The fixture list has "banana" (only `ban` match).
    s.type_text("ban").await?;
    s.wait_text(r"1/10").await?;
    s.wait_idle(150).await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("banana")?;
    snap.assert_outline_contains("1/10")?;

    s.press("<c-c>").await?;
    s.die().await?;
    Ok(())
}

#[tokio::test]
async fn bwrap_fzf_select_outputs_selection_to_stdout() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_fzf_select", fixtures::FZF).await?;
    // Wrap the pipe so we can see fzf's stdout (the selection) land
    // back in the pane after exit.
    s.spawn([
        "bash",
        "-c",
        "cat /fixtures/fruits.txt | fzf --layout=reverse --no-mouse --height=100% | tee /tmp/picked; echo FZF_DONE",
    ])
    .await?;
    s.wait_text(r"10/10").await?;
    s.wait_idle(150).await?;

    // Filter to the one match, then Enter selects.
    s.type_text("ban").await?;
    s.wait_text(r"1/10").await?;
    s.wait_idle(120).await?;
    s.press("<cr>").await?;
    // After fzf exits, the bash pipeline's `tee` echoes the selection
    // and `FZF_DONE` confirms the wrapper finished.
    s.wait_text("FZF_DONE").await?;
    s.wait_idle(150).await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("banana")?;
    snap.assert_outline_contains("FZF_DONE")?;
    assert_ne!(
        snap.state().unwrap_or(""),
        "alt_screen_tui",
        "after fzf exits the pane drops back to normal screen"
    );

    s.die().await?;
    Ok(())
}
