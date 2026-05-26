//! bwrap-backend port of the vimtutor walk-through. Mirrors
//! `vimtutor_walkthrough.rs`; only the runtime differs.

#![cfg(feature = "bwrap")]

use agent_tui_integration::bwrap::{BwrapScenario, fixtures};
use anyhow::Result;

#[tokio::test]
async fn bwrap_vimtutor_walkthrough_lessons_1_1_to_1_3() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_vimtutor_walkthrough", fixtures::VIM).await?;

    s.spawn(["bash", "-c", "vimtutor; echo 'tutor-finished'"])
        .await?;

    // Vimtutor opens at a welcome banner ("Welcome to the VIM Tutor")
    // that does NOT yet include the lesson headers — those live further
    // down the buffer. Anchor on the banner text + alt-screen state, not
    // on "Lesson 1.1".
    s.wait_text(r"VIM Tutor").await?;
    s.wait_idle(200).await?;

    {
        let snap = s.snapshot().await?;
        assert_eq!(snap.state().unwrap_or(""), "alt_screen_tui");
        snap.assert_outline_contains("ATTENTION")?;
    }

    // Search forward to the Lesson 1.3 anchor (vimtutor-recommended
    // navigation pattern between sections).
    s.press("/Lesson 1.3<cr>").await?;
    s.wait_text(r"Lesson 1\.3").await?;
    s.wait_idle(150).await?;

    {
        let snap = s.snapshot().await?;
        snap.assert_outline_contains("Lesson 1.3")?;
    }

    s.press(":q!<cr>").await?;
    s.wait_text("tutor-finished").await?;
    s.wait_idle(120).await?;

    let snap = s.snapshot().await?;
    assert_ne!(
        snap.state().unwrap_or(""),
        "alt_screen_tui",
        "after :q! the bash wrapper should be visible (not alt-screen)"
    );
    snap.assert_outline_contains("tutor-finished")?;

    s.die().await?;
    Ok(())
}
