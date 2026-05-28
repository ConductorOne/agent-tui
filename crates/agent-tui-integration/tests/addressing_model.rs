//! End-to-end coverage of the addressing model through a real PTY.
//!
//! Exercises:
//!  - `wait --ref '@vim.cmdline[focused]'` fires when vim's cmdline opens
//!  - `wait --ref` + `--gone` fires when the cmdline closes
//!  - `snapshot --select '@vim.statusline'` returns just the statusline
//!  - `snapshot --select` with `--all` returns multiple matches
//!  - `press --to '@vim.buffer' '…'` succeeds via identity routing
//!
//! Bwrap-backend; same scenarios exist for the docker backend in
//! `vim_bwrap.rs` (the addressing-model rewrites of `vim_basic.rs`).

#![cfg(feature = "bwrap")]

use agent_tui_integration::bwrap::{BwrapScenario, fixtures};
use anyhow::Result;
use serde_json::Value;

#[tokio::test]
async fn select_returns_only_the_targeted_node() -> Result<()> {
    let mut s = BwrapScenario::new("select_one_node", fixtures::VIM).await?;
    s.spawn(["vim", "/fixtures/sample.txt"]).await?;
    s.wait_ref("@vim.buffer").await?;
    s.wait_idle(120).await?;

    // `snapshot --select` via the raw CLI; the scenario helper doesn't
    // expose a typed wrapper yet, so build the argv directly.
    let env = s
        .run_cli_raw(&[
            "snapshot".into(),
            "--mode".into(),
            "outline".into(),
            "--select".into(),
            "@vim.statusline".into(),
        ])
        .await?;
    let data = env.get("data").expect("snapshot envelope has data");
    let nodes = data
        .get("outline")
        .and_then(|o| o.get("nodes"))
        .and_then(Value::as_array)
        .expect("filtered outline has nodes array");
    assert_eq!(
        nodes.len(),
        1,
        "expected exactly one matched node; got {nodes:?}"
    );
    assert_eq!(
        nodes[0].get("ref").and_then(Value::as_str),
        Some("@vim.statusline")
    );

    s.press(":q!<cr>").await?;
    s.die().await?;
    Ok(())
}

#[tokio::test]
async fn wait_ref_gone_fires_when_cmdline_closes() -> Result<()> {
    let mut s = BwrapScenario::new("cmdline_open_close", fixtures::VIM).await?;
    s.spawn(["vim", "/fixtures/sample.txt"]).await?;
    s.wait_ref("@vim.buffer").await?;
    s.wait_idle(120).await?;

    // Open command mode → wait for cmdline focus.
    s.press(":").await?;
    s.wait_ref("@vim.cmdline[focused]").await?;
    // ESC closes the cmdline → focus drops.
    s.press("<esc>").await?;
    s.wait_ref_gone("@vim.cmdline[focused]").await?;

    s.press(":q!<cr>").await?;
    s.die().await?;
    Ok(())
}

#[tokio::test]
async fn press_with_to_routes_through_identity_when_no_custom_route() -> Result<()> {
    // The vim adapter uses the default `route` (identity), so `--to`
    // resolves the selector and writes the bytes straight to the
    // PTY. Effect on the buffer should match a plain `press` call.
    let mut s = BwrapScenario::new("press_to_identity", fixtures::VIM).await?;
    s.spawn(["vim", "/fixtures/sample.txt"]).await?;
    s.wait_ref("@vim.buffer").await?;
    s.wait_idle(120).await?;

    let env = s
        .run_cli_raw(&[
            "press".into(),
            "--to".into(),
            "@vim.buffer".into(),
            "ihello-from-routing<esc>".into(),
        ])
        .await?;
    let data = env.get("data").expect("press envelope has data");
    assert_eq!(
        data.get("routed").and_then(Value::as_bool),
        Some(true),
        "press --to should report routed=true; envelope = {env:#?}"
    );

    s.wait_ref(r"@vim.file[value=modified]").await?;
    let snap = s.snapshot().await?;
    snap.assert_outline_contains("hello-from-routing")?;

    s.press(":q!<cr>").await?;
    s.die().await?;
    Ok(())
}
