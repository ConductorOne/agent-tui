//! GNU nano scenarios — modeless editor with Ctrl-key shortcuts. The
//! antithesis of vim: every key types literally except `Ctrl-*`, and
//! the chrome lives in a 2-row footer rather than a single statusline.
//!
//! Workflows:
//!  - Open file — title bar carries the filename, footer carries the
//!    `^X Exit` shortcut. Anchor on both.
//!  - Modify the buffer — title bar adds `Modified`.
//!  - Save with `^O`<cr> — status line shows `[ Wrote N lines ]` and
//!    `Modified` clears.

#![cfg(feature = "bwrap")]

use agent_tui_integration::bwrap::{BwrapScenario, fixtures};
use anyhow::Result;

/// nano args we pass in every scenario. We keep the 2-row help footer
/// OFF (`-x`) because with `-I` (no rc) nano expands to a long extended
/// help block that pushes the buffer off the rendered viewport in a
/// 24-row terminal. `-w` disables auto hard-wrap, `-R` runs restricted
/// (no shell-out, no read-other-files), `--ignorercfiles` mirrors `-I`
/// without the extended-help side effect.
const NANO_FLAGS: &[&str] = &["--ignorercfiles", "--nohelp", "-w", "-R"];

#[tokio::test]
async fn bwrap_nano_opens_file_with_chrome() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_nano_opens", fixtures::NANO).await?;
    // Seed /work first so nano can write back (the fixture path is on
    // a ro-bind, and restricted mode prevents nano from changing the
    // edit target after launch).
    std::fs::write(
        s.scratch_host_path().join("hello.txt"),
        "hello world from nano\nsecond line\nthird line\n",
    )?;
    let mut argv = vec!["nano"];
    argv.extend_from_slice(NANO_FLAGS);
    argv.push("/work/hello.txt");
    s.spawn(argv.iter().copied()).await?;

    // The title bar shows "GNU nano" + the filename. The footer shows
    // the Ctrl-key shortcut menu; `^X Exit` is the most stable anchor.
    s.wait_text("GNU nano").await?;
    s.wait_idle(150).await?;

    let snap = s.snapshot().await?;
    assert_eq!(snap.state().unwrap_or(""), "alt_screen_tui");
    snap.assert_outline_contains("hello world from nano")?;
    // Title bar anchor — always row 0, always literal text.
    snap.assert_outline_contains("GNU nano")?;

    // Cleanly exit via Ctrl-X; buffer is unmodified, no save prompt.
    s.press("<c-x>").await?;
    s.die().await?;
    Ok(())
}

#[tokio::test]
async fn bwrap_nano_typed_buffer_shows_modified() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_nano_modified", fixtures::NANO).await?;
    // Use a unique seed string so we know nano finished loading before
    // we start typing.
    std::fs::write(s.scratch_host_path().join("hello.txt"), "originalseed\n")?;
    let mut argv = vec!["nano"];
    argv.extend_from_slice(NANO_FLAGS);
    argv.push("/work/hello.txt");
    s.spawn(argv.iter().copied()).await?;
    // Wait for both the chrome AND the seeded content to land — nano
    // briefly shows `[ Reading … ]` mid-load and our typed character
    // would race that if we only waited for the chrome.
    s.wait_text("GNU nano").await?;
    s.wait_text("originalseed").await?;
    s.wait_idle(250).await?;

    // Type a unique marker character. The typed prefix lands at the
    // cursor (row 1 col 0); nano also writes a right-aligned
    // `Modified` flag onto row 0 with CUP-then-SGR.
    s.type_text("Z").await?;
    s.wait_text("Modified").await?;
    s.wait_idle(200).await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("Zoriginalseed")?;
    snap.assert_outline_contains("Modified")?;

    // Discard + exit: ^X then N (don't save).
    s.press("<c-x>").await?;
    s.wait_text("Save modified buffer").await?;
    s.press("N").await?;
    s.die().await?;
    Ok(())
}

#[tokio::test]
async fn bwrap_nano_save_clears_modified() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_nano_save", fixtures::NANO).await?;
    std::fs::write(s.scratch_host_path().join("hello.txt"), "originalseed\n")?;
    let mut argv = vec!["nano"];
    argv.extend_from_slice(NANO_FLAGS);
    argv.push("/work/hello.txt");
    s.spawn(argv.iter().copied()).await?;
    s.wait_text("GNU nano").await?;
    s.wait_text("originalseed").await?;
    s.wait_idle(250).await?;

    // Modify the buffer with a unique prefix.
    s.type_text("ZZmod-").await?;
    s.wait_text("Modified").await?;
    s.wait_idle(200).await?;

    // Save with ^O. nano prompts `File Name to Write: <filename>`,
    // and after the write the `Modified` flag clears + the status
    // line shows `[ Wrote N lines ]`.
    s.press("<c-o>").await?;
    s.wait_text("File Name to Write").await?;
    s.press("<cr>").await?;
    s.wait_text(r"Wrote").await?;
    s.wait_idle(250).await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("Wrote")?;
    snap.assert_outline_contains("ZZmod-originalseed")?;
    let outline_text = serde_json::to_string(snap.envelope())?;
    assert!(
        !outline_text.contains("Modified"),
        "Modified flag should clear after save"
    );

    // Disk-side check — proves the keypresses reached nano AND nano
    // wrote the buffer back through the /work bind. End-to-end.
    let written = std::fs::read_to_string(s.scratch_host_path().join("hello.txt"))?;
    assert!(
        written.starts_with("ZZmod-"),
        "expected file to start with ZZmod-; got {written:?}"
    );

    s.press("<c-x>").await?;
    s.die().await?;
    Ok(())
}
