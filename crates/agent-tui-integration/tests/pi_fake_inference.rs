//! End-to-end: Pi (earendil-works/pi) AI CLI driven by agent-tui,
//! talking to our in-process fake-inference server.
//!
//! Proves the full real-world AI-CLI loop works hermetically:
//!   `FakeServer` (host TCP) ← Pi (sandbox bwrap) ← agent-tui daemon
//!     ← test driver
//!
//! Supply-chain story: Pi binary is sha256-pinned to v0.75.5 in
//! `fixtures/pi/Dockerfile`. Its baseURL is overridden via a per-test
//! `models.json` that points at the localhost `FakeServer` port. No
//! `Anthropic` / `OpenAI` / etc. credentials needed — no network
//! leaving the host.

#![cfg(feature = "bwrap")]

use agent_tui_integration::bwrap::{BwrapScenario, fixtures};
use agent_tui_integration::fake_inference::{FakeServer, Script};
use anyhow::Result;

/// Write a Pi models.json at the scratch path the fixture's
/// `PI_CODING_AGENT_DIR` env points at, configuring a single fake
/// provider that talks to `server_url`.
fn write_pi_models_json(scratch: &std::path::Path, server_url: &str) -> Result<()> {
    let dir = scratch.join("pi-agent");
    std::fs::create_dir_all(&dir)?;
    let cfg = serde_json::json!({
        "providers": {
            "fake": {
                "baseUrl": format!("{server_url}/v1"),
                "api": "openai-completions",
                "apiKey": "test-key",
                "compat": {
                    "supportsDeveloperRole": false,
                    "supportsReasoningEffort": false,
                },
                "models": [
                    { "id": "fake-model" }
                ]
            }
        }
    });
    std::fs::write(dir.join("models.json"), serde_json::to_string_pretty(&cfg)?)?;
    // Also create the sessions dir so Pi doesn't bail on the first
    // session write.
    std::fs::create_dir_all(scratch.join("pi-sessions"))?;
    Ok(())
}

/// The simplest scenario: Pi says hello back. Verifies the full chain
/// works without any fancy streaming behavior.
#[tokio::test]
async fn pi_says_hello_via_fake_inference() -> Result<()> {
    let mut s = BwrapScenario::new("pi_says_hello", fixtures::PI).await?;

    let server = FakeServer::start(Script::new().say("FAKE_HELLO_PI")).await?;
    write_pi_models_json(s.scratch_host_path(), &server.url())?;

    // `pi --print` runs non-interactively, echoes the assistant reply
    // to stdout, then exits. We spawn it through the daemon's PTY so
    // we can snapshot the rendered output.
    s.spawn([
        "pi",
        "--provider",
        "fake",
        "--model",
        "fake-model",
        "--print",
        "say hello",
    ])
    .await?;

    s.wait_text("FAKE_HELLO_PI").await?;
    s.wait_idle(150).await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("FAKE_HELLO_PI")?;

    s.die().await?;
    Ok(())
}

/// Streaming: server returns the reply in 3 chunks; Pi assembles + renders.
/// The visible result is the same as the single-shot case, but the
/// path through the engine exercises mid-stream snapshot stability.
#[tokio::test]
async fn pi_streams_chunked_reply() -> Result<()> {
    let mut s = BwrapScenario::new("pi_streams_chunked", fixtures::PI).await?;

    let server =
        FakeServer::start(Script::new().stream(["FAKE_", "STREAMED_", "RESPONSE"])).await?;
    write_pi_models_json(s.scratch_host_path(), &server.url())?;

    s.spawn([
        "pi",
        "--provider",
        "fake",
        "--model",
        "fake-model",
        "--print",
        "stream me something",
    ])
    .await?;

    s.wait_text("FAKE_STREAMED_RESPONSE").await?;
    s.wait_idle(150).await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("FAKE_STREAMED_RESPONSE")?;

    s.die().await?;
    Ok(())
}

/// Multi-turn: two script entries, two `pi` invocations against the
/// same fake server. Proves the `FakeServer`'s script cursor advances
/// correctly across connections.
#[tokio::test]
async fn pi_multi_request_advances_script() -> Result<()> {
    let mut s = BwrapScenario::new("pi_multi_request", fixtures::PI).await?;

    let server = FakeServer::start(
        Script::new()
            .say("FIRST_REPLY_X1Y2Z3")
            .say("SECOND_REPLY_A4B5C6"),
    )
    .await?;
    write_pi_models_json(s.scratch_host_path(), &server.url())?;

    // First call.
    s.spawn([
        "pi",
        "--provider",
        "fake",
        "--model",
        "fake-model",
        "--print",
        "what's first?",
    ])
    .await?;
    s.wait_text("FIRST_REPLY_X1Y2Z3").await?;
    s.wait_idle(150).await?;
    {
        let snap = s.snapshot().await?;
        snap.assert_outline_contains("FIRST_REPLY_X1Y2Z3")?;
    }
    s.die().await?;

    // Second call — same scenario, same server, second script slot.
    s.spawn([
        "pi",
        "--provider",
        "fake",
        "--model",
        "fake-model",
        "--print",
        "and second?",
    ])
    .await?;
    s.wait_text("SECOND_REPLY_A4B5C6").await?;
    s.wait_idle(150).await?;

    let snap = s.snapshot().await?;
    snap.assert_outline_contains("SECOND_REPLY_A4B5C6")?;

    s.die().await?;
    Ok(())
}
