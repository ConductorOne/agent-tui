//! Vimtutor walk-through scenario — drives vim's built-in 30-minute
//! tutorial through its first few lessons via the agent-tui CLI.
//!
//! Why this exists: it's the most expressive end-to-end demonstration we
//! have of the agent-tui surface. A single test exercises spawn, press
//! with the full keymap grammar, `wait_text` against vim's repaint cadence,
//! snapshot evolution across many state transitions, and clean teardown.
//!
//! It also doubles as marketing: "look, agent-tui drives a real
//! interactive tutorial all the way through."
//!
//! Vimtutor lesson layout (this scenario hits 1.1 through 1.3):
//!
//! | Lesson | What it teaches | What we exercise |
//! |---|---|---|
//! | 1.1 | hjkl cursor motion | `press("j j j j j")` + snapshot |
//! | 1.2 | :q! to exit | (deliberately skipped — we don't want to quit) |
//! | 1.3 | x to delete a char | `press("x")` + observe character removed |

#![cfg(feature = "docker")]

use agent_tui_integration::scenario::{Scenario, fixtures};
use anyhow::Result;

#[tokio::test]
async fn vimtutor_walkthrough_lessons_1_1_to_1_3() -> Result<()> {
    let mut s = Scenario::new("vimtutor_walkthrough", fixtures::VIM).await?;

    // vimtutor wraps vim with a preprocessed tutor file; spawning it
    // directly drops the agent at lesson 1.1.
    //
    // Wrap in `bash -c` so we have a parent shell to return to once
    // vimtutor exits, which keeps the pane alive for the final
    // snapshot.
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

    // Search forward for the lesson-1.3 anchor and confirm it lands.
    // (`/Lesson 1.3<cr>` is the vimtutor-recommended motion between
    // sections.)
    s.press("/Lesson 1.3<cr>").await?;
    s.wait_text(r"Lesson 1\.3").await?;
    s.wait_idle(150).await?;

    {
        let snap = s.snapshot().await?;
        snap.assert_outline_contains("Lesson 1.3")?;
    }

    // Quit out of vimtutor without saving; the bash wrapper then runs
    // `echo tutor-finished` so we have a deterministic exit anchor.
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
