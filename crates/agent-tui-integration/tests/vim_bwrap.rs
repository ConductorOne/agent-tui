//! bwrap-backend port of the addressing-model vim scenarios.
//!
//! Same assertions as `vim_basic.rs`, different runtime. Gated on
//! the `bwrap` feature so `cargo test --workspace` still skips these.

#![cfg(feature = "bwrap")]

use agent_tui_integration::bwrap::{BwrapScenario, fixtures};
use anyhow::Result;

#[tokio::test]
async fn bwrap_vim_opens_file_and_shows_content() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_vim_opens_file", fixtures::VIM).await?;
    s.spawn(["vim", "/fixtures/sample.txt"]).await?;
    s.wait_ref("@vim.buffer").await?;
    s.wait_idle(120).await?;

    let snap = s.snapshot().await?;
    let state = snap.state().unwrap_or("");
    assert_eq!(
        state, "alt_screen_tui",
        "vim should classify as alt-screen TUI; got {state:?}"
    );
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

#[tokio::test]
async fn bwrap_vim_edit_save_round_trip() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_vim_edit_save", fixtures::VIM).await?;
    let src = "/fixtures/sample.txt";
    let dst = "/work/sample.txt";
    s.spawn(["bash", "-c", &format!("cp {src} {dst}; vim {dst}")])
        .await?;
    s.wait_ref("@vim.buffer").await?;
    s.wait_idle(120).await?;

    s.press("i hello-from-bwrap<esc>").await?;
    s.wait_ref(r"@vim.file[value=modified]").await?;

    s.press(":w<cr>").await?;
    s.wait_ref(r"@vim.statusline[name~=/written/]").await?;

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
    s.wait_ref("@vim.buffer").await?;
    s.wait_idle(120).await?;

    s.press("/").await?;
    s.wait_ref("@vim.cmdline[focused]").await?;
    s.type_text("foo two").await?;
    s.press("<cr>").await?;
    s.wait_ref_gone("@vim.cmdline[focused]").await?;
    s.wait_idle(120).await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("foo two")?;

    s.press(":q!<cr>").await?;
    s.die().await?;
    Ok(())
}
