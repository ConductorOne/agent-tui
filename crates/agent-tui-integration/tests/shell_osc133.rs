//! Cycle I2: bash + `FinalTerm` OSC 133 end-to-end scenarios.
//!
//! Catches regression classes that synthetic OSC-byte unit tests can't:
//!  - real bash emits the integration markers when sourcing the
//!    `/etc/profile.d/osc133.sh` we ship in the fixture image
//!  - the PTY reader's OSC 133 scanner survives bash's own escape-heavy
//!    output (color, cursor positioning) interleaved with the markers
//!  - the daemon's classifier transitions Shell -> Running -> Shell as a
//!    real command starts and finishes

#![cfg(feature = "docker")]

use agent_tui_integration::scenario::{Scenario, fixtures};
use anyhow::Result;

/// A freshly-prompted bash with OSC 133 integration should classify as
/// `PaneState::Shell` after the first `A` marker arrives.
#[tokio::test]
async fn bash_with_osc133_classifies_as_shell() -> Result<()> {
    let mut s = Scenario::new("bash_osc133_shell", fixtures::SHELL).await?;
    s.spawn(["bash", "--login", "-i"]).await?;
    // Wait for the prompt text from the fixture's PS1; by the time it
    // renders, the A marker has already flowed through.
    s.wait_text(r"agent-tui-fixture\$").await?;

    let snap = s.snapshot().await?;
    let state = snap.state().unwrap_or("");
    assert_eq!(
        state,
        "shell",
        "expected shell state with OSC 133 A marker present; got {state:?}\n\
         outline: {:#?}",
        snap.envelope()
    );

    s.die().await?;
    Ok(())
}

/// While a foreground command is running, the C marker should switch
/// classification to `PaneState::Running`.
#[tokio::test]
async fn bash_running_command_classifies_as_running() -> Result<()> {
    let mut s = Scenario::new("bash_osc133_running", fixtures::SHELL).await?;
    s.spawn(["bash", "--login", "-i"]).await?;
    s.wait_text(r"agent-tui-fixture\$").await?;

    // Start a 5s sleep so we can sample while it's still running. The
    // DEBUG trap fires before the sleep starts, emitting C, which
    // upgrades state to Running.
    s.press("sleep 5<cr>").await?;
    // Give the DEBUG-trap C marker a tick to flow through.
    s.wait_idle(150).await?;

    let snap = s.snapshot().await?;
    let state = snap.state().unwrap_or("");
    assert_eq!(
        state,
        "running",
        "expected running state during `sleep 5`; got {state:?}\n\
         outline: {:#?}",
        snap.envelope()
    );

    s.die().await?;
    Ok(())
}

/// Once the foreground command exits, the D marker (with exit status)
/// should land and classification snaps back to Shell.
#[tokio::test]
async fn bash_returns_to_shell_after_command_finishes() -> Result<()> {
    let mut s = Scenario::new("bash_osc133_return_to_shell", fixtures::SHELL).await?;
    s.spawn(["bash", "--login", "-i"]).await?;
    s.wait_text(r"agent-tui-fixture\$").await?;

    // Quick echo + return: D marker fires the moment PROMPT_COMMAND
    // runs again. We wait for the *second* prompt line to land before
    // sampling state.
    s.press("echo hello-cmd<cr>").await?;
    s.wait_text(r"hello-cmd").await?;
    // Bash needs to repaint the prompt; idle-150 catches that quiescence.
    s.wait_idle(150).await?;

    let snap = s.snapshot().await?;
    let state = snap.state().unwrap_or("");
    assert_eq!(
        state,
        "shell",
        "expected shell state after command finishes; got {state:?}\n\
         outline: {:#?}",
        snap.envelope()
    );

    s.die().await?;
    Ok(())
}
