//! htop scenarios — proves agent-tui survives a live-redraw TUI.
//!
//! htop is the first fixture that mutates its own screen on a timer.
//! We launch with `-d 50` (5-second refresh) so `wait_idle` has a
//! large quiet window between repaints. `-C` is mono mode (color
//! attrs drop, smaller snapshot diff). `--no-mouse` keeps OSC
//! mouse-tracking bytes out of the cell stream.
//!
//! Scenarios:
//!  - Launch + snapshot the F-key bar and process-list header.
//!  - Toggle Tree view (F5) — visible difference: `F5Sorted` label.
//!  - Open the F2 setup screen and bail out via Esc.

#![cfg(feature = "bwrap")]

use agent_tui_integration::bwrap::{BwrapScenario, fixtures};
use anyhow::Result;

const HTOP_ARGS: &[&str] = &["htop", "-d", "50", "-C", "--no-mouse"];

#[tokio::test]
async fn bwrap_htop_renders_process_list_and_fkeys() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_htop_renders", fixtures::HTOP).await?;
    s.spawn(HTOP_ARGS.iter().copied()).await?;
    // Anchor on the F-key bar text — stable across htop 3.x.
    s.wait_text(r"F10").await?;
    s.wait_idle(250).await?;

    let snap = s.snapshot().await?;
    assert_eq!(
        snap.state().unwrap_or(""),
        "alt_screen_tui",
        "htop runs in alt-screen TUI"
    );
    // The F-key bar is the most reliable anchor — the column header
    // row's `PID USER ...` text moves around depending on viewport
    // width because htop reflows columns to fit; the F-key bar's
    // contents are stable in any 80+ width terminal.
    snap.assert_outline_contains("F10Quit")?;
    snap.assert_outline_contains("F1Help")?;

    s.press("q").await?;
    s.die().await?;
    Ok(())
}

#[tokio::test]
async fn bwrap_htop_tree_toggle_changes_fkey_label() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_htop_tree_toggle", fixtures::HTOP).await?;
    s.spawn(HTOP_ARGS.iter().copied()).await?;
    s.wait_text(r"F10").await?;
    s.wait_idle(250).await?;

    // F5 toggles Tree view. In flat mode the process rows have no
    // hierarchy glyphs; in tree mode they get ASCII tree characters
    // (`|--` after `set line-graphics = ascii`, or `├─`/`│` with
    // unicode glyphs by default). htop 3.2 doesn't relabel the F-key
    // bar entry — the tree glyph is the most reliable visual cue.
    s.press("<f5>").await?;
    // Wait for the tree glyph to land in some process row.
    s.wait_text(r"├─|\|--").await?;
    s.wait_idle(200).await?;

    let snap = s.snapshot().await?;
    let text = serde_json::to_string(snap.envelope())?;
    assert!(
        text.contains("├─") || text.contains("|--"),
        "expected tree glyph after F5; outline contains no tree marker"
    );

    s.press("q").await?;
    s.die().await?;
    Ok(())
}

#[tokio::test]
async fn bwrap_htop_setup_screen_opens_and_closes() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_htop_setup", fixtures::HTOP).await?;
    s.spawn(HTOP_ARGS.iter().copied()).await?;
    s.wait_text(r"F10").await?;
    s.wait_idle(250).await?;

    // F2 enters the Setup screen — the "Setup" title appears and the
    // process list disappears.
    s.press("<f2>").await?;
    s.wait_text("Setup").await?;
    s.wait_idle(200).await?;

    {
        let snap = s.snapshot().await?;
        snap.assert_outline_contains("Setup")?;
    }

    // Esc returns to the main view; the F-key bar with `F1Help`
    // text reappears.
    s.press("<esc>").await?;
    s.wait_text("F1Help").await?;
    s.wait_idle(200).await?;

    s.press("q").await?;
    s.die().await?;
    Ok(())
}
