//! End-to-end verification that the `VimAdapter` is active for vim panes.
//!
//! The adapter framework's whole point is structured outlines — the snapshot
//! should carry `adapter: "vim"` and a `role: "mode"` node, not the
//! generic header/body/footer triple. These tests prove the daemon's
//! adapter registry + auto-detect + outline call site all wire together
//! when a real vim is driven through a real PTY.

#![cfg(feature = "bwrap")]

use agent_tui_integration::bwrap::{BwrapScenario, fixtures};
use anyhow::Result;
use serde_json::Value;

/// Pull the `outline` object out of a snapshot envelope.
fn outline(snap_env: &Value) -> &Value {
    snap_env.get("outline").expect("snapshot has outline")
}

/// Find a node with the given `role` in the outline.
fn find_node<'a>(outline_obj: &'a Value, role: &str) -> Option<&'a Value> {
    outline_obj
        .get("nodes")?
        .as_array()?
        .iter()
        .find(|n| n.get("role").and_then(Value::as_str) == Some(role))
}

#[tokio::test]
async fn vim_pane_uses_vim_adapter() -> Result<()> {
    let mut s = BwrapScenario::new("vim_adapter_active", fixtures::VIM).await?;
    s.spawn(["vim", "/fixtures/sample.txt"]).await?;
    s.wait_text(r"sample\.txt").await?;
    s.wait_idle(150).await?;

    let snap = s.snapshot().await?;
    let env = snap.envelope();
    let ol = outline(env);
    assert_eq!(
        ol.get("adapter").and_then(Value::as_str),
        Some("vim"),
        "expected adapter=vim; outline = {ol:#?}"
    );

    s.press(":q!<cr>").await?;
    s.die().await?;
    Ok(())
}

#[tokio::test]
async fn vim_insert_mode_shows_in_outline() -> Result<()> {
    let mut s = BwrapScenario::new("vim_insert_mode_outline", fixtures::VIM).await?;
    s.spawn(["vim", "/fixtures/sample.txt"]).await?;
    s.wait_text(r"sample\.txt").await?;
    s.wait_idle(150).await?;

    // Enter insert mode; vim's showmode prints `-- INSERT --` on the
    // command-line row.
    s.press("i").await?;
    s.wait_text("INSERT").await?;
    s.wait_idle(150).await?;

    let snap = s.snapshot().await?;
    let mode = find_node(outline(snap.envelope()), "mode").expect("mode node");
    assert_eq!(
        mode.get("name").and_then(Value::as_str),
        Some("insert"),
        "mode node = {mode:#?}"
    );

    s.press("<esc>:q!<cr>").await?;
    s.die().await?;
    Ok(())
}

#[tokio::test]
async fn vim_command_mode_carries_command_line() -> Result<()> {
    let mut s = BwrapScenario::new("vim_command_mode_outline", fixtures::VIM).await?;
    s.spawn(["vim", "/fixtures/sample.txt"]).await?;
    s.wait_text(r"sample\.txt").await?;
    s.wait_idle(150).await?;

    // Type `:set ` (no <cr>!) so vim stays in command mode with the
    // text on the command-line row.
    s.press(":set ").await?;
    s.wait_text(":set").await?;
    s.wait_idle(120).await?;

    let snap = s.snapshot().await?;
    let mode = find_node(outline(snap.envelope()), "mode").expect("mode node");
    assert_eq!(mode.get("name").and_then(Value::as_str), Some("command"));
    let cmdline = mode
        .get("value")
        .and_then(Value::as_str)
        .expect("command-line text in value");
    assert!(
        cmdline.starts_with(":set"),
        "expected command-line to start with :set, got {cmdline:?}"
    );

    s.press("<esc>:q!<cr>").await?;
    s.die().await?;
    Ok(())
}

#[tokio::test]
async fn vim_modified_file_marks_status_node() -> Result<()> {
    let mut s = BwrapScenario::new("vim_modified_status", fixtures::VIM).await?;
    // Seed the writable scratch dir on the host BEFORE spawn, so we can
    // run vim directly (without a `bash -c "cp && vim"` wrapper that
    // would push detection into the lower-confidence wrapper branch).
    std::fs::write(s.scratch_host_path().join("sample.txt"), "first line\n")?;
    s.spawn(["vim", "/work/sample.txt"]).await?;
    s.wait_text(r"sample\.txt").await?;
    s.wait_idle(150).await?;

    // Insert a character and ESC back to normal mode — vim adds [+] to
    // the statusline once the buffer is modified.
    s.press("ihello<esc>").await?;
    s.wait_text(r"\[\+\]").await?;
    s.wait_idle(150).await?;

    let snap = s.snapshot().await?;
    let file = find_node(outline(snap.envelope()), "file").expect("file node");
    assert_eq!(
        file.get("value").and_then(Value::as_str),
        Some("modified"),
        "expected file.value=modified; node = {file:#?}"
    );

    s.press(":q!<cr>").await?;
    s.die().await?;
    Ok(())
}
