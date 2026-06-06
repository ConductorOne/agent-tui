//! cov-5 (gap #5, P1): `press --to <selector>` routed adapter-delivery e2e.
//!
//! The `--to` branch of `handlers/input.rs` resolves a selector against the
//! attached adapter's outline, asks the adapter's `route()` to translate the
//! keystroke into `RoutedStep`s, and runs them through `execute_routed_steps`
//! — a path distinct from the default no-`--to` write (`write_with_barrier`).
//! A generic pane has no routable refs, so this branch is invisible to the
//! daemon-lane tests; it is only exercised by a real adapter under the sandbox
//! (bwrap) lane. This test drives a real `vim` and proves:
//!
//!  1. `press --to '@vim.buffer'` reports the routed-only response contract
//!     (`routed=true`, and `bytes_requested == bytes_written` — fields the
//!     default write path never emits), i.e. delivery genuinely went through
//!     `route()`/`execute_routed_steps`, not the default path; and
//!  2. the routed keystrokes land an observable marker in the target node; and
//!  3. `--to` actually RESOLVES the selector — a non-matching selector is
//!     rejected (`ROUTING_UNSUPPORTED`, "matched no node") and its keystrokes
//!     never reach the PTY (the buffer is unchanged).
//!
//! Discrimination (why this fails if routing regressed): if the `--to` branch
//! fell back to the default write, `data.routed`/`bytes_requested` would be
//! absent (assertion 1 fails). If `route()` produced wrong/empty steps, the
//! marker would not appear (assertion 2 fails). If `--to` stopped resolving the
//! selector and wrote to the master regardless, the non-matching press would
//! SUCCEED and "should-not-land" would appear in the buffer (assertion 3 fails
//! on both the missing error and the leaked marker).
//!
//! Docker-backend siblings of the addressing-model scenarios live in
//! `vim_bwrap.rs`; the identity-route happy path is also touched by
//! `addressing_model.rs::press_with_to_routes_through_identity_when_no_custom_route`
//! — this test adds the routed response contract and the selector-resolution
//! discrimination on top.

#![cfg(feature = "bwrap")]

use agent_tui_integration::bwrap::{BwrapScenario, fixtures};
use anyhow::Result;
use serde_json::Value;

#[tokio::test]
async fn press_to_routes_keystrokes_through_adapter_to_target_node() -> Result<()> {
    let mut s = BwrapScenario::new("routed_press_to", fixtures::VIM).await?;
    s.spawn(["vim", "/fixtures/sample.txt"]).await?;
    // Wait until the vim adapter is detected and the routable buffer ref exists.
    s.wait_ref("@vim.buffer").await?;
    s.wait_idle(120).await?;

    // (1) Routed delivery through `route()`: enter insert mode, type a marker,
    //     leave insert mode — all routed to the @vim.buffer target node.
    let env = s
        .run_cli_raw(&[
            "press".into(),
            "--to".into(),
            "@vim.buffer".into(),
            "irouted-cov5-marker<esc>".into(),
        ])
        .await?;
    assert_eq!(
        env.get("success").and_then(Value::as_bool),
        Some(true),
        "routed press must succeed; envelope = {env:#?}"
    );
    let data = env.get("data").expect("press envelope has data");
    // Routed-only contract: the default write path emits neither `routed` nor
    // `bytes_requested`, so these prove `execute_routed_steps` ran.
    assert_eq!(
        data.get("routed").and_then(Value::as_bool),
        Some(true),
        "press --to must report routed=true (routed path, not default write); data = {data:#?}"
    );
    let requested = data
        .get("bytes_requested")
        .and_then(Value::as_u64)
        .expect("routed response carries bytes_requested");
    let written = data
        .get("bytes_written")
        .and_then(Value::as_u64)
        .expect("routed response carries bytes_written");
    assert!(requested > 0, "routed keystroke must have a byte length");
    assert_eq!(
        written, requested,
        "identity route must deliver every requested byte (written {written} != requested {requested})"
    );

    // (2) Observable effect: the routed keystrokes actually reached the buffer.
    s.wait_ref(r"@vim.file[value=modified]").await?;
    let snap = s.snapshot().await?;
    snap.assert_outline_contains("routed-cov5-marker")?;

    // (3) `--to` genuinely resolves the selector: a non-matching selector is
    //     rejected and its keystrokes never reach the PTY. A router that
    //     ignored `--to` and wrote to the master would instead inject
    //     "should-not-land" into the buffer.
    let miss = s
        .run_cli_raw(&[
            "press".into(),
            "--to".into(),
            "@vim.zzznotarealnode".into(),
            "ishould-not-land<esc>".into(),
        ])
        .await?;
    assert_eq!(
        miss.get("success").and_then(Value::as_bool),
        Some(false),
        "a non-matching --to selector must be rejected, not silently written; envelope = {miss:#?}"
    );
    let err_msg = miss
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        err_msg.contains("matched no node"),
        "rejection must explain the selector matched no node; got: {err_msg:?}"
    );
    // The rejected keystrokes must not have leaked to the buffer.
    let after = s.snapshot().await?;
    assert!(
        after.assert_outline_contains("should-not-land").is_err(),
        "rejected routed keystrokes must NOT reach the buffer; outline = {:#?}",
        after.envelope()
    );

    s.press(":q!<cr>").await?;
    s.die().await?;
    Ok(())
}
