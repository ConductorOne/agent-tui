//! End-to-end tests for `agent-tui mcp serve` — the MCP (Model Context
//! Protocol) server that exposes the CLI surface as JSON-RPC tools over
//! stdio.
//!
//! This is the adapter/tool IPC boundary an MCP client (Claude Desktop /
//! Code) drives. The unit tests in `mcp.rs` cover `build_command` + the
//! tool schemas, but the *server* — the stdio read/dispatch/respond loop,
//! the `initialize`/`tools/list`/`tools/call` handlers, the real daemon
//! round-trip in `call_tool`, and the JSON-RPC error responses — is only
//! reachable by speaking real JSON-RPC to the real process. So these
//! tests spawn the built `agent-tui mcp serve`, write real JSON-RPC lines
//! to its stdin, and assert on the NDJSON it writes back, with a **real
//! daemon + real PTY child** behind `tools/call`. No mock client, no
//! faked transport.
//!
//! Coverage this reaches that `cargo test --workspace` otherwise misses:
//!  - `mcp.rs::serve` (the stdio loop + notification handling)
//!  - `dispatch` for `initialize` / `tools/list` / `tools/call`
//!  - `call_tool` (the live `client::one_shot` round-trip + the
//!    `content`/`isError` envelope wrapping)
//!  - the error half: `-32601` (method not found) and `-32602`
//!    (invalid params) JSON-RPC error responses
//!
//! The whole exchange is bounded (`drive`): all requests are written,
//! stdin is closed (EOF ends the serve loop), and the process is killed +
//! the test fails if it overruns. No sleeps-then-assert, no wall-clock
//! races — responses are matched by JSON-RPC `id`.
//!
//! Gated `cfg(unix)`: the `tools/call` children are POSIX shell utilities.

#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_agent-tui")
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Isolated daemon home, mirroring the `run_verb` / `streaming` harnesses.
struct Harness {
    socket_dir: PathBuf,
    state_home: PathBuf,
}

impl Harness {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(format!("/tmp/at-mcp-{}-{n}-{tag}", std::process::id()));
        let socket_dir = root.join("s");
        let state_home = root.join("x");
        std::fs::create_dir_all(&socket_dir).expect("mkdir socket dir");
        std::fs::create_dir_all(&state_home).expect("mkdir state home");
        Self {
            socket_dir,
            state_home,
        }
    }

    fn base_cmd(&self) -> Command {
        let mut c = Command::new(bin());
        c.arg("--socket-dir")
            .arg(&self.socket_dir)
            .arg("--session")
            .arg("t")
            .env("XDG_STATE_HOME", &self.state_home)
            .env("AGENT_TUI_SOCKET_DIR", &self.socket_dir)
            .env(
                "AGENT_TUI_MONITOR_PARENT_PID",
                std::process::id().to_string(),
            );
        c
    }

    /// Drive one `mcp serve` session: write every request as an NDJSON
    /// line, close stdin (EOF terminates the serve loop), then collect
    /// the response lines. Killed + fails if it overruns `max`.
    fn drive(&self, requests: &[Value], max: Duration) -> Vec<Value> {
        let mut child = self
            .base_cmd()
            .arg("mcp")
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn agent-tui mcp serve");

        {
            let mut stdin = child.stdin.take().expect("child stdin");
            for req in requests {
                writeln!(stdin, "{req}").expect("write request");
            }
            // Drop stdin → EOF → serve loop drains buffered requests,
            // writes their responses, then returns.
        }

        let start = Instant::now();
        loop {
            if child.try_wait().expect("try_wait").is_some() {
                break;
            }
            if start.elapsed() > max {
                let _ = child.kill();
                let _ = child.wait();
                panic!("mcp serve did not exit within {max:?} — stdio exchange wedged");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let out = child.wait_with_output().expect("wait_with_output");
        let stdout = String::from_utf8_lossy(&out.stdout);
        stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str::<Value>(l).unwrap_or_else(|e| {
                    panic!(
                        "non-JSON response line {l:?}: {e}; stderr={:?}",
                        String::from_utf8_lossy(&out.stderr)
                    )
                })
            })
            .collect()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self
            .base_cmd()
            .args(["daemon", "shutdown", "--force"])
            .output();
        let _ = std::fs::remove_dir_all(self.socket_dir.parent().unwrap_or(&self.socket_dir));
    }
}

/// Find the response whose JSON-RPC `id` equals `id`.
fn by_id(responses: &[Value], id: i64) -> &Value {
    responses
        .iter()
        .find(|r| r["id"].as_i64() == Some(id))
        .unwrap_or_else(|| panic!("no response with id {id}; got {responses:?}"))
}

/// Full happy-path handshake + a real `tools/call` round-trip:
/// `initialize` → `notifications/initialized` (no reply) → `tools/list`
/// → `tools/call spawn` (lazy-spawns the real daemon + PTY child) →
/// `tools/call wait` (blocks server-side until the child's text appears —
/// race-free, no sleep) → `tools/call snapshot` (real outline) →
/// `tools/call die`.
#[test]
fn mcp_handshake_lists_tools_and_drives_a_real_pane() {
    let h = Harness::new("happy");
    let reqs = vec![
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
            "name":"spawn",
            "arguments":{"argv":["/bin/sh","-c","printf hello-mcp; sleep 2"]}
        }}),
        // Block until the child's output lands so the snapshot is
        // deterministic — no sleep, no race.
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
            "name":"wait","arguments":{"text":"hello-mcp","max":8000}
        }}),
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{
            "name":"snapshot","arguments":{"mode":"outline"}
        }}),
        json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{
            "name":"die","arguments":{}
        }}),
    ];
    let resp = h.drive(&reqs, Duration::from_secs(25));

    // The notification (no id) must produce NO response: 6 ids → 6 replies.
    assert_eq!(
        resp.len(),
        6,
        "one reply per request with an id, none for the notification; got {resp:?}"
    );

    // initialize
    let init = by_id(&resp, 1);
    assert_eq!(init["jsonrpc"], "2.0");
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(init["result"]["serverInfo"]["name"], "agent-tui");

    // tools/list — schemas present, including the ones we call.
    let tools = by_id(&resp, 2)["result"]["tools"]
        .as_array()
        .expect("tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for want in ["spawn", "snapshot", "die", "wait", "press"] {
        assert!(
            names.contains(&want),
            "tools/list missing {want:?}: {names:?}"
        );
    }

    // tools/call spawn — success envelope, not an error.
    let spawn = by_id(&resp, 3);
    assert_eq!(
        spawn["result"]["isError"], false,
        "spawn tool errored: {spawn:?}"
    );

    // tools/call wait — matched the text, so not an error.
    let waited = by_id(&resp, 4);
    assert_eq!(
        waited["result"]["isError"], false,
        "wait tool errored: {waited:?}"
    );

    // tools/call snapshot — the content text is the agent-tui envelope;
    // it must carry the child's visible output ("hello-mcp").
    let snap = by_id(&resp, 5);
    assert_eq!(
        snap["result"]["isError"], false,
        "snapshot tool errored: {snap:?}"
    );
    let text = snap["result"]["content"][0]["text"]
        .as_str()
        .expect("snapshot content text");
    let envelope: Value =
        serde_json::from_str(text).expect("snapshot content must be a JSON envelope");
    assert_eq!(envelope["success"], true, "snapshot envelope: {envelope:?}");
    assert!(
        text.contains("hello-mcp"),
        "snapshot outline should contain the child's output; got {text}"
    );

    // die — success.
    assert_eq!(by_id(&resp, 6)["result"]["isError"], false);
}

/// Error half (the coldest part of `mcp.rs`): an unknown method maps to
/// JSON-RPC `-32601` (method not found); a `tools/call` for an unknown
/// tool maps to `-32602` (invalid params). Both must come back as
/// well-formed JSON-RPC `error` objects, not crashes or success replies.
#[test]
fn mcp_reports_jsonrpc_errors_for_bad_requests() {
    let h = Harness::new("err");
    let reqs = vec![
        json!({"jsonrpc":"2.0","id":10,"method":"resources/nonexistent","params":{}}),
        json!({"jsonrpc":"2.0","id":11,"method":"tools/call","params":{
            "name":"nonesuch-tool","arguments":{}
        }}),
    ];
    let resp = h.drive(&reqs, Duration::from_secs(15));
    assert_eq!(
        resp.len(),
        2,
        "both bad requests must get a reply: {resp:?}"
    );

    let unknown_method = by_id(&resp, 10);
    assert!(
        unknown_method.get("result").is_none(),
        "unknown method must not return a result: {unknown_method:?}"
    );
    assert_eq!(
        unknown_method["error"]["code"], -32601,
        "unknown method → -32601: {unknown_method:?}"
    );

    let bad_tool = by_id(&resp, 11);
    assert!(
        bad_tool.get("result").is_none(),
        "bad tools/call must not return a result: {bad_tool:?}"
    );
    assert_eq!(
        bad_tool["error"]["code"], -32602,
        "unknown tool → invalid params -32602: {bad_tool:?}"
    );
}
