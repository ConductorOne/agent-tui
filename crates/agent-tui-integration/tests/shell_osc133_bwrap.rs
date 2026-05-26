//! bwrap-backend port of the shell + OSC 133 scenarios. Mirrors
//! `shell_osc133.rs` assertion-for-assertion; only the runtime differs.

#![cfg(feature = "bwrap")]

use agent_tui_integration::bwrap::{BwrapScenario, fixtures};
use anyhow::Result;

#[tokio::test]
async fn bwrap_bash_with_osc133_classifies_as_shell() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_bash_osc133_shell", fixtures::SHELL).await?;
    s.spawn(["bash", "--login", "-i"]).await?;
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

#[tokio::test]
async fn bwrap_bash_running_command_classifies_as_running() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_bash_osc133_running", fixtures::SHELL).await?;
    s.spawn(["bash", "--login", "-i"]).await?;
    s.wait_text(r"agent-tui-fixture\$").await?;

    s.press("sleep 5<cr>").await?;
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

#[tokio::test]
async fn bwrap_bash_returns_to_shell_after_command_finishes() -> Result<()> {
    let mut s = BwrapScenario::new("bwrap_bash_osc133_return_to_shell", fixtures::SHELL).await?;
    s.spawn(["bash", "--login", "-i"]).await?;
    s.wait_text(r"agent-tui-fixture\$").await?;

    s.press("echo hello-cmd<cr>").await?;
    s.wait_text(r"hello-cmd").await?;
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
