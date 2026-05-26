//! Cycle I3: vim end-to-end scenarios.
//!
//! Each scenario exercises a different vim subsystem the agent-tui flow
//! has to handle correctly:
//!  - `vim_opens_file_and_shows_content` — initial render + alt-screen
//!    state classifier on TUIs
//!  - `vim_edit_save_round_trip` — insert mode, ESC, ex command, statusline
//!  - `vim_search_finds_target` — `/pattern<cr>` flow + buffer text wait
//!  - `vim_quit_with_force` — alt-screen tear-down on `:q!`

#![cfg(feature = "docker")]

use agent_tui_integration::scenario::{Scenario, fixtures};
use anyhow::Result;

/// Open `/fixtures/sample.txt` in vim, wait for the file to render, and
/// confirm the snapshot picks up both the buffer content and the
/// alt-screen state classification.
#[tokio::test]
async fn vim_opens_file_and_shows_content() -> Result<()> {
    let mut s = Scenario::new("vim_opens_file", fixtures::VIM).await?;
    s.spawn(["vim", "/fixtures/sample.txt"]).await?;
    // Wait for the statusline (laststatus=2 in vimrc) to render — by
    // then alt-screen toggle + first paint are both done.
    s.wait_text(r"sample\.txt").await?;
    s.wait_idle(120).await?;

    let snap = s.snapshot().await?;
    let state = snap.state().unwrap_or("");
    assert_eq!(
        state, "alt_screen_tui",
        "vim should classify as alt-screen TUI; got {state:?}"
    );

    // Three seeded lines plus statusline; just check one substring.
    snap.assert_outline_contains("first line")?;

    s.press(":q!<cr>").await?;
    s.die().await?;
    Ok(())
}

/// `i hello<esc>:w<cr>` writes the file. Statusline transitions through
/// `--INSERT--` then back to `[+]` after save.
#[tokio::test]
async fn vim_edit_save_round_trip() -> Result<()> {
    let mut s = Scenario::new("vim_edit_save", fixtures::VIM).await?;
    s.spawn(["vim", "/fixtures/sample.txt"]).await?;
    s.wait_text(r"sample\.txt").await?;
    s.wait_idle(120).await?;

    s.press("i hello-from-agent-tui<esc>").await?;
    // The `+` modifier appears on the statusline after a modification.
    s.wait_text(r"\[\+\]").await?;

    s.press(":w<cr>").await?;
    // After save, vim echoes `"sample.txt" N lines, M bytes written`.
    s.wait_text(r"written").await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("hello-from-agent-tui")?;

    s.press(":q!<cr>").await?;
    s.die().await?;
    Ok(())
}

/// `/foo<cr>` lands on the first match; cursor + visible text confirm it.
#[tokio::test]
async fn vim_search_finds_target() -> Result<()> {
    let mut s = Scenario::new("vim_search", fixtures::VIM).await?;
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

/// `:q!` tears down alt-screen; the post-quit pane state should drop out
/// of `AltScreenTui`.
#[tokio::test]
async fn vim_quit_releases_alt_screen() -> Result<()> {
    let mut s = Scenario::new("vim_quit_releases_alt_screen", fixtures::VIM).await?;
    // Vim wrapped in a bash so there's still a pane after vim exits.
    s.spawn(["bash", "-c", "vim /fixtures/sample.txt; echo bye"])
        .await?;
    s.wait_text(r"sample\.txt").await?;
    s.wait_idle(120).await?;

    {
        let snap = s.snapshot().await?;
        assert_eq!(snap.state().unwrap_or(""), "alt_screen_tui");
    }

    s.press(":q!<cr>").await?;
    s.wait_text("bye").await?;
    s.wait_idle(120).await?;

    let snap = s.snapshot().await?;
    let state = snap.state().unwrap_or("");
    assert_ne!(
        state, "alt_screen_tui",
        "after :q! the pane should no longer be alt-screen"
    );

    s.die().await?;
    Ok(())
}
