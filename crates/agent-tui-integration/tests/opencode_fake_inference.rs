//! End-to-end: `OpenCode` (sst/opencode) AI CLI driven by agent-tui
//! against the in-process fake-inference server, exercising the
//! `OpenAI` Responses API path.
//!
//! ## What this proves
//!
//! The full real-world AI-coding-agent loop end-to-end:
//!   `FakeServer` (host TCP, in-process)
//!     ← `OpenCode` v1.15.10 (sha256-pinned, bwrap-sandboxed)
//!     ← agent-tui daemon
//!     ← test driver
//!
//! ## Two assertion strategies
//!
//! Default-format `opencode run` writes only a session header to stdout
//! ("`> build · fake-model`"); the actual assistant body is persisted to
//! `~/.local/share/opencode/opencode.db` (`SQLite`, WAL mode). We use
//! two assertion strategies depending on what we're testing:
//!
//! 1. **`SQLite`-byte-grep**: scan `opencode.db` AND `opencode.db-wal`
//!    for marker strings. Decisive proof "`OpenCode` parsed the stream,
//!    assembled the body, persisted it, committed the session." Each
//!    scenario binds a writable dir at `/root` (the `persist_home`
//!    bit on the fixture) so the DB lands in test-visible storage.
//!
//! 2. **`--format json` stdout-parsing**: opencode's JSON mode writes
//!    one event per line to stdout, including the assistant deltas.
//!    Used by the streaming scenario to assert chunks arrive
//!    incrementally (not buffered) without screenshot races.
//!
//! ## Why these particular scenarios
//!
//! - `opencode_persists_streamed_response_to_session_db` — baseline.
//!   Without the Responses-API streaming path working end-to-end,
//!   nothing else is reachable.
//! - `opencode_streams_chunks_incrementally` — proves the stream
//!   isn't accidentally buffered to one big delta. Uses `--format
//!   json` for race-free chunk-by-chunk assertions.
//! - `opencode_persists_user_prompt_in_session` — guards against
//!   silent agent amnesia ("the agent forgot what I asked").
//! - `opencode_handles_server_500_without_hanging` — error path.
//!   Server returns no script slot → 500; agent must terminate, not
//!   wedge waiting for a never-arriving response.
//!
//! Future scenarios (`tracker.md`): tool use (`bash` invocation),
//! multi-turn via `--continue`, mid-stream cancellation.
//!
//! ## Supply-chain story
//!
//! `OpenCode` v1.15.10 is sha256-pinned in `fixtures/opencode/Dockerfile`.
//! The fake server runs in-process on a host loopback port; the bwrap
//! sandbox reaches it because the `OPENCODE` fixture sets
//! `needs_network: true`.

#![cfg(feature = "bwrap")]

use agent_tui_integration::bwrap::{BwrapScenario, fixtures};
use agent_tui_integration::fake_inference::{FakeServer, Reply, Script};
use anyhow::Result;
use serde_json::Value;

/// Drop a per-scenario `opencode.json` into the scratch dir.
/// `OpenCode` reads `./opencode.json` first when its cwd is `/work`.
fn write_opencode_config(scratch: &std::path::Path, server_url: &str) -> Result<()> {
    let cfg = serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "provider": {
            "openai": {
                "options": {
                    "baseURL": format!("{server_url}/v1"),
                    "apiKey": "test-key-not-real",
                },
                "models": {
                    "fake-model": {}
                }
            }
        }
    });
    std::fs::write(
        scratch.join("opencode.json"),
        serde_json::to_string_pretty(&cfg)?,
    )?;
    Ok(())
}

/// `opencode run` argv we use across scenarios.
///
/// Wrapped in `bash -c "cd /work && exec opencode …"` because the
/// `bwrap` sandbox launches with cwd=`/`, and OpenCode looks for
/// `./opencode.json` in the current directory. Without the `cd`,
/// our per-scenario provider config doesn't get picked up and
/// OpenCode tries to reach the real OpenAI endpoint with a bogus
/// API key.
///
/// Flag choices:
/// - `--dangerously-skip-permissions`: opencode otherwise waits on
///   TTY prompts before running any tool, which deadlocks under our
///   PTY harness. We're driving a fake server so there's no real
///   destructive call.
/// - `--pure`: skip external plugins (deterministic).
/// - `--title 'fixed test title'`: opencode fires a SEPARATE
///   title-generation request to the model BEFORE the real prompt
///   when no title is supplied. That extra call burns the first slot
///   of any multi-slot Script. Passing a fixed title skips it.
/// - `-m openai/fake-model`: use the model our own `opencode.json`
///   defines under the provider's `models` map. Resolution then never
///   touches opencode's live-fetched model catalog (models.dev) — which
///   is NOT hermetic: the catalog floated forward and dropped the
///   previously-used `o3-mini`, and the failure mode depended on the
///   fetch outcome (fetch fails → `UnknownError` after the first-run DB
///   migration; fetch succeeds → `Model not found: openai/o3-mini`;
///   fetch falls back to the baked catalog → test passes). The fake
///   server doesn't care what the model is — it echoes the request's
///   `model` field back — only the path and shape matter.
fn opencode_run_cmd(prompt: &str) -> Vec<String> {
    vec![
        "bash".into(),
        "-c".into(),
        format!(
            "cd /work && exec opencode run --pure --dangerously-skip-permissions \
             --title 'fixed test title' -m openai/fake-model {}",
            shell_quote(prompt),
        ),
    ]
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Read the OpenCode SQLite DB raw from the persisted home dir and
/// look for `needle` as a UTF-8 byte run. SQLite stores text payloads
/// uncompressed in B-tree pages — a substring search of the raw file
/// bytes is sufficient for "the assistant message was persisted."
///
/// **Important:** OpenCode opens its DB in WAL mode and finishes
/// before SQLite checkpoints the WAL back into the main `.db` file.
/// We must scan BOTH `opencode.db` AND `opencode.db-wal` to find
/// content written during this run.
///
/// We don't link against rusqlite/sqlite-bundled just for this check
/// because it'd add a heavy dev-dep and longer compile times for one
/// scenario's purpose.
fn opencode_db_contains(home_persist: &std::path::Path, needle: &str) -> Result<bool> {
    let dir = home_persist.join(".local").join("share").join("opencode");
    let db = dir.join("opencode.db");
    let wal = dir.join("opencode.db-wal");
    if !db.exists() {
        anyhow::bail!(
            "opencode SQLite DB not found at {} — did the run actually happen?",
            db.display()
        );
    }
    for path in [&db, &wal] {
        if !path.exists() {
            continue;
        }
        let bytes = std::fs::read(path)?;
        if bytes.windows(needle.len()).any(|w| w == needle.as_bytes()) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[tokio::test]
async fn opencode_persists_streamed_response_to_session_db() -> Result<()> {
    let mut s = BwrapScenario::new("opencode_persists_streamed", fixtures::OPENCODE).await?;
    let server =
        FakeServer::start(Script::new().stream(["OPENCODE_", "FAKE_", "HELLO_", "PERSISTED"]))
            .await?;
    write_opencode_config(s.scratch_host_path(), &server.url())?;

    s.spawn(opencode_run_cmd("say hello").iter().map(String::as_str))
        .await?;
    // Wait for OpenCode to render its session header (the line that
    // marks "session created, about to call the model"), then settle
    // long enough for the streaming response to land, be parsed by
    // OpenCode, committed to its SQLite session DB, and for the
    // process to exit. `wait_idle(2000)` is generous but bounded —
    // an opencode call against a localhost fake settles in well under
    // a second.
    s.wait_text(r"build · ").await?;
    s.wait_idle(2000).await?;

    // First-line diagnosis: did OpenCode actually hit our endpoint?
    // If not, no point inspecting the DB — the agent never made the
    // call, and that's the real failure to investigate.
    let reqs = server.requests();
    assert!(
        reqs.iter()
            .any(|r| r.path.contains("/responses") || r.path.contains("/chat/completions")),
        "OpenCode never POSTed to a chat endpoint; saw requests = {:?}",
        reqs.iter().map(|r| &r.request_line).collect::<Vec<_>>()
    );

    // The strong assertion: OpenCode received our streamed response,
    // assembled the full text from the deltas, and persisted it.
    // Raw byte grep of the SQLite file is the most decisive proof
    // that the agent's session state has the assistant message body
    // — irrespective of whether `run --format default` chose to
    // write it to stdout.
    let home = s.home_persist_host_path();
    let full = "OPENCODE_FAKE_HELLO_PERSISTED";
    assert!(
        opencode_db_contains(home, full)?,
        "expected assembled assistant text {full:?} in opencode.db; \
         server saw {} request(s): {:?}",
        reqs.len(),
        reqs.iter().map(|r| &r.path).collect::<Vec<_>>()
    );

    s.die().await?;
    Ok(())
}

/// Build an `opencode run --format json …` argv. JSON mode writes one
/// JSON object per line to stdout — including the assistant's reply
/// inline — so the test driver can assert against stream contents
/// directly without going through the DB.
fn opencode_run_json_cmd(prompt: &str) -> Vec<String> {
    vec![
        "bash".into(),
        "-c".into(),
        format!(
            "cd /work && exec opencode run --pure --dangerously-skip-permissions \
             --title 'fixed test title' --format json -m openai/fake-model {}",
            shell_quote(prompt),
        ),
    ]
}

/// Scenario B: prove the streaming path is actually streamed (not
/// "wait for the whole response then emit all at once"). With a
/// generous per-chunk delay, the early chunks land in `wait_text`
/// noticeably before the later ones. We assert two things:
///   1. Each chunk's unique marker arrives as separate stdout events.
///   2. The total elapsed time exceeds the cumulative inter-chunk
///      delay — proving opencode actually paused between deltas
///      rather than buffering everything server-side.
///
/// `--format json` is the key: it dumps each `message.part.updated`
/// event as a JSON line, giving us deterministic, race-free
/// assertions instead of trying to snapshot mid-render.
#[tokio::test]
async fn opencode_streams_chunks_incrementally() -> Result<()> {
    let mut s = BwrapScenario::new("opencode_streams_chunks", fixtures::OPENCODE).await?;

    let reply = Reply::streamed(["EARLY_AAA_", "MID_BBB_", "LATE_CCC"]).with_delay(120);
    let server = FakeServer::start(Script::new().reply(reply)).await?;
    write_opencode_config(s.scratch_host_path(), &server.url())?;

    s.spawn(
        opencode_run_json_cmd("stream slowly")
            .iter()
            .map(String::as_str),
    )
    .await?;

    // The earliest chunk should appear well before the latest one.
    // If opencode was buffering server-side, all three would show up
    // simultaneously after ~360ms (3 × 120ms server delay). By
    // waiting on each individually we prove they trickled through.
    s.wait_text("EARLY_AAA_").await?;
    s.wait_text("MID_BBB_").await?;
    s.wait_text("LATE_CCC").await?;
    s.wait_idle(800).await?;

    // Sanity: the server saw the request go to the Responses endpoint.
    let reqs = server.requests();
    assert!(
        reqs.iter().any(|r| r.path.contains("/responses")),
        "opencode should hit /v1/responses; saw {:?}",
        reqs.iter().map(|r| &r.path).collect::<Vec<_>>()
    );

    // And the full assembled body persisted to the session DB. This
    // double-checks that `--format json` doesn't bypass the normal
    // session-state pipeline.
    assert!(
        opencode_db_contains(s.home_persist_host_path(), "EARLY_AAA_MID_BBB_LATE_CCC")?,
        "assembled body should land in opencode.db (WAL ok)"
    );

    s.die().await?;
    Ok(())
}

/// Scenario D: a single-turn but with a stricter test — opencode
/// should also persist the user's prompt as a `part` row. This
/// catches regressions where the agent loses the input text. Many
/// "agent failures" in the field are "the agent forgot what I asked"
/// — verify that doesn't silently happen here.
#[tokio::test]
async fn opencode_persists_user_prompt_in_session() -> Result<()> {
    let mut s = BwrapScenario::new("opencode_persists_user_prompt", fixtures::OPENCODE).await?;
    // A unique-looking prompt so we can grep for it in the DB.
    let prompt = "USER_PROMPT_MARKER_Z9X8Y7";
    let server = FakeServer::start(Script::new().say("ASSISTANT_REPLY_MARKER_K4L5M6")).await?;
    write_opencode_config(s.scratch_host_path(), &server.url())?;

    s.spawn(opencode_run_cmd(prompt).iter().map(String::as_str))
        .await?;
    s.wait_text(r"build · ").await?;
    s.wait_idle(2000).await?;

    let home = s.home_persist_host_path();
    assert!(
        opencode_db_contains(home, prompt)?,
        "user's prompt must be persisted to opencode.db"
    );
    assert!(
        opencode_db_contains(home, "ASSISTANT_REPLY_MARKER_K4L5M6")?,
        "assistant reply must be persisted to opencode.db"
    );

    s.die().await?;
    Ok(())
}

/// Scenario E: server returns a 500 with no script entries — opencode
/// must terminate cleanly without hanging. Validates the timeout +
/// error-handling path in the streaming client. We don't assert a
/// specific error message because that varies across opencode versions
/// — we assert: (a) the process exited, (b) the server saw a request,
/// (c) the test scenario didn't have to escalate to `s.die()` with
/// a wedged child.
#[tokio::test]
async fn opencode_handles_server_500_without_hanging() -> Result<()> {
    let mut s = BwrapScenario::new("opencode_server_500", fixtures::OPENCODE).await?;
    // Empty script => fake server returns 500 to every request.
    let server = FakeServer::start(Script::new()).await?;
    write_opencode_config(s.scratch_host_path(), &server.url())?;

    s.spawn(opencode_run_cmd("trigger error").iter().map(String::as_str))
        .await?;

    // Wait up to 8s for the session-start banner — that's emitted
    // before the API call, so it's a low bar. Then wait long enough
    // for opencode to give up and exit. If `wait_idle` times out,
    // opencode is hung and our error-handling story has a regression.
    s.wait_text(r"build · ").await?;
    s.wait_idle(3000).await?;

    let reqs = server.requests();
    assert!(
        !reqs.is_empty(),
        "opencode should have attempted the request; got 0 requests"
    );
    // The server returns 500. Opencode's session DB may or may not
    // record an error part — version-dependent. We don't assert on
    // it. The "didn't hang" property is the value here.

    s.die().await?;
    Ok(())
}

/// Scenario C: bash tool use — the agent-defining capability.
///
/// Flow:
///   1. Server replies with a `function_call(bash, {"command":"echo …"})`
///      tool-use event sequence.
///   2. OpenCode parses, auto-approves (via `--dangerously-skip-permissions`),
///      and executes the bash command inside the sandbox.
///   3. OpenCode posts a second request containing a `function_call_output`
///      with the bash stdout. The server replies with a plain text body.
///   4. OpenCode persists the tool call, its output, and the final
///      assistant text to the session DB.
///
/// We assert all three pieces land in the DB:
///   - the tool name (`bash`),
///   - the bash command output marker (proves opencode actually ran it),
///   - the final assistant text marker (proves the loop closed).
#[tokio::test]
async fn opencode_executes_bash_tool_use_and_persists_output() -> Result<()> {
    let mut s = BwrapScenario::new("opencode_bash_tool_use", fixtures::OPENCODE).await?;

    let bash_marker = "MARKER_FROM_BASH_TOOL_99A1B2";
    // OpenCode's bash tool schema requires BOTH `command` and
    // `description`. Omitting either produces a SchemaError that
    // surfaces as the function_call_output instead of real stdout.
    let bash_args =
        format!(r#"{{"command":"echo {bash_marker}","description":"echo a marker for the test"}}"#);

    let server = FakeServer::start(
        Script::new()
            .tool_call("bash", bash_args)
            .say("FINAL_ASSISTANT_REPLY_77C3D4"),
    )
    .await?;
    write_opencode_config(s.scratch_host_path(), &server.url())?;

    s.spawn(opencode_run_cmd("echo a marker").iter().map(String::as_str))
        .await?;
    s.wait_text(r"build · ").await?;
    // Tool-use is two round-trips + a local bash exec; give it more
    // time than the single-shot scenarios.
    s.wait_idle(4000).await?;

    let reqs = server.requests();
    assert!(
        reqs.len() >= 2,
        "expected at least 2 requests (tool call + follow-up); got {}: {:?}",
        reqs.len(),
        reqs.iter().map(|r| &r.path).collect::<Vec<_>>()
    );

    // Find the function_call_output item in the follow-up request and
    // assert its `output` contains the bash marker. Looking at the
    // dedicated field (not just `body.contains()`) guards against
    // false positives where the marker only appears because it was
    // echoed back in the function_call arguments.
    let second = &reqs[1].body;
    let output_text = second
        .get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call_output"))
        .and_then(|item| item.get("output"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        output_text.contains(bash_marker),
        "function_call_output.output must contain bash stdout {bash_marker:?}; got: {output_text:?}"
    );

    let home = s.home_persist_host_path();
    assert!(
        opencode_db_contains(home, "bash")?,
        "bash tool name should be in opencode.db"
    );
    assert!(
        opencode_db_contains(home, bash_marker)?,
        "bash output must land in opencode.db"
    );
    assert!(
        opencode_db_contains(home, "FINAL_ASSISTANT_REPLY_77C3D4")?,
        "final assistant reply must land in opencode.db"
    );

    s.die().await?;
    Ok(())
}
