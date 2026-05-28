//! Cycle I3: vim end-to-end scenarios — addressing-model edition.
//!
//! Every assertion uses `wait --ref` against `@vim.*` durable refs
//! instead of `wait --text` regex over rendered cells. See the
//! `addressing` skill and `docs/addressing-rfc.md` §2.2.

#![cfg(feature = "docker")]

use agent_tui_integration::scenario::{Scenario, fixtures};
use anyhow::Result;

/// Open `/fixtures/sample.txt` in vim, wait for the buffer node,
/// confirm the `@vim` subtree carries the file content.
#[tokio::test]
async fn vim_opens_file_and_shows_content() -> Result<()> {
    let mut s = Scenario::new("vim_opens_file", fixtures::VIM).await?;
    s.spawn(["vim", "/fixtures/sample.txt"]).await?;

    // Wait for the buffer to render. `@vim.buffer` is durable — same
    // ref every frame for as long as vim owns the pane.
    s.wait_ref("@vim.buffer").await?;
    // Idle barrier covers the gap between "buffer exists" and "first
    // paint settled" — alt-screen flips before vim writes content.
    s.wait_idle(120).await?;

    let snap = s.snapshot().await?;
    let state = snap.state().unwrap_or("");
    assert_eq!(
        state, "alt_screen_tui",
        "vim should classify as alt-screen TUI; got {state:?}"
    );
    // Hit-test the @vim.buffer node directly instead of stringifying
    // the whole outline.
    let buf = snap
        .find("@vim.buffer")
        .expect("@vim.buffer should exist after wait_ref");
    let body = buf.get("name").and_then(|n| n.as_str()).unwrap_or("");
    assert!(
        body.contains("first line"),
        "buffer should contain seeded content; got {body:?}"
    );

    s.press(":q!<cr>").await?;
    s.die().await?;
    Ok(())
}

/// `i hello<esc>:w<cr>` writes the file. Wait on `@vim.file[value=modified]`
/// for the `+` marker and `@vim.statusline[name~=/written/]` for the
/// save echo — both are structural, neither false-fires on the typed
/// command before vim executes it.
#[tokio::test]
async fn vim_edit_save_round_trip() -> Result<()> {
    let mut s = Scenario::new("vim_edit_save", fixtures::VIM).await?;
    s.spawn(["vim", "/fixtures/sample.txt"]).await?;
    s.wait_ref("@vim.buffer").await?;
    s.wait_idle(120).await?;

    s.press("i hello-from-agent-tui<esc>").await?;
    // Modified flag is now a structured field on the file node.
    s.wait_ref(r"@vim.file[value=modified]").await?;

    s.press(":w<cr>").await?;
    // After save, the statusline echoes `"sample.txt" N lines, M bytes written`.
    s.wait_ref(r"@vim.statusline[name~=/written/]").await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("hello-from-agent-tui")?;

    s.press(":q!<cr>").await?;
    s.die().await?;
    Ok(())
}

/// `/foo<cr>` lands on the first match. We wait for the cmdline to
/// open, type, then wait for it to close — that's the moment vim has
/// actually executed the search.
#[tokio::test]
async fn vim_search_finds_target() -> Result<()> {
    let mut s = Scenario::new("vim_search", fixtures::VIM).await?;
    s.spawn(["vim", "/fixtures/search-target.txt"]).await?;
    s.wait_ref("@vim.buffer").await?;
    s.wait_idle(120).await?;

    s.press("/").await?;
    s.wait_ref("@vim.cmdline[focused]").await?;
    s.type_text("foo two").await?;
    s.press("<cr>").await?;
    // Once vim runs the search, the cmdline drops focus.
    s.wait_ref_gone("@vim.cmdline[focused]").await?;
    s.wait_idle(120).await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("foo two")?;

    s.press(":q!<cr>").await?;
    s.die().await?;
    Ok(())
}

/// `:q!` tears down the alt-screen; the post-quit pane drops out of
/// `alt_screen_tui` state and (since vim was launched from a bash)
/// the shell adapter takes over.
#[tokio::test]
async fn vim_quit_releases_alt_screen() -> Result<()> {
    let mut s = Scenario::new("vim_quit_releases_alt_screen", fixtures::VIM).await?;
    s.spawn(["bash", "-c", "vim /fixtures/sample.txt; echo bye"])
        .await?;
    s.wait_ref("@vim.buffer").await?;
    s.wait_idle(120).await?;

    {
        let snap = s.snapshot().await?;
        assert_eq!(snap.state().unwrap_or(""), "alt_screen_tui");
    }

    s.press(":q!<cr>").await?;
    // After quit, @vim is gone from the outline — that's the
    // post-quit signal under the new model.
    s.wait_ref_gone("@vim").await?;
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
