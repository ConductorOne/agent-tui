//! End-to-end test for the MCP server mode.
//!
//! Spawns `agent-tui mcp serve` as a subprocess, drives it via a real
//! JSON-RPC-over-stdio conversation (the same envelope shape Claude
//! Desktop / Claude Code would use), and verifies:
//!  - `initialize` returns server info + capabilities
//!  - `tools/list` includes the core tool names
//!  - `tools/call name=spawn` actually starts a pane in the underlying
//!    daemon (lazy-spawned via the same socket)
//!  - The pane's snapshot survives the full round-trip
//!
//! No bwrap / Docker dependency — this exercises the agent-tui binary
//! on the host directly (the child process inside the pane is a plain
//! `bash -c "echo ..."`).

#![cfg(any(feature = "bwrap", feature = "docker"))]

use std::process::Stdio;
use std::time::Duration;

use agent_tui_integration::{agent_tui_binary, workspace_root};
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

struct McpClient {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    next_id: u64,
}

impl McpClient {
    // `async` for symmetry with the other methods; the constructor itself
    // is sync, but tests `.await` it.
    #[allow(clippy::unused_async)]
    async fn start(socket_dir: &std::path::Path, state_home: &std::path::Path) -> Result<Self> {
        let bin = agent_tui_binary()?;
        let mut child = Command::new(&bin)
            .arg("--socket-dir")
            .arg(socket_dir)
            .args(["mcp", "serve"])
            .env("XDG_STATE_HOME", state_home)
            .env("AGENT_TUI_SOCKET_DIR", socket_dir)
            // Allow anything — these tests spawn whatever fixture they need.
            .env("AGENT_TUI_ALLOWED_BINARIES", "*")
            // Tie any lazy-spawned daemon's lifetime to this test
            // process so a panic or SIGKILL'd test runner doesn't
            // orphan a daemon (Layer 1 of the cleanup architecture).
            .env(
                "AGENT_TUI_MONITOR_PARENT_PID",
                std::process::id().to_string(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn agent-tui mcp serve")?;
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let stdout = BufReader::new(stdout).lines();
        Ok(Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        })
    }

    async fn send(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let s = serde_json::to_string(&frame)?;
        self.stdin.write_all(s.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;

        // Read until we see a response with our id.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let line = tokio::time::timeout_at(deadline, self.stdout.next_line())
                .await
                .map_err(|_| anyhow!("timeout waiting for MCP response to {method}"))?
                .context("read stdout")?
                .ok_or_else(|| anyhow!("EOF on stdout"))?;
            let v: Value =
                serde_json::from_str(&line).with_context(|| format!("parse MCP line: {line}"))?;
            if v.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = v.get("error") {
                    return Err(anyhow!("MCP error: {err}"));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            // Skip non-matching responses (e.g. notifications, late
            // arrivals from previous tests).
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let frame = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let s = serde_json::to_string(&frame)?;
        self.stdin.write_all(s.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn shutdown(mut self) -> Result<()> {
        // Drop stdin so the server sees EOF and exits cleanly.
        drop(self.stdin);
        // Give the server up to 5 seconds to exit on EOF.
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        Ok(())
    }
}

fn temp_root() -> std::path::PathBuf {
    let nonce = uuid::Uuid::new_v4().to_string();
    let nonce: String = nonce.chars().take(8).collect();
    let root = std::env::temp_dir().join(format!("at-mcp-{nonce}"));
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[tokio::test]
async fn mcp_initialize_handshake() -> Result<()> {
    let root = temp_root();
    let socket_dir = root.join("s");
    let state_home = root.join("x");
    std::fs::create_dir_all(&socket_dir)?;
    std::fs::create_dir_all(&state_home)?;

    let mut c = McpClient::start(&socket_dir, &state_home).await?;
    let result = c
        .send(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "agent-tui-integration", "version": "0.1.0"},
            }),
        )
        .await?;

    assert_eq!(
        result.get("protocolVersion").and_then(Value::as_str),
        Some("2024-11-05")
    );
    let server = result.get("serverInfo").expect("serverInfo");
    assert_eq!(
        server.get("name").and_then(Value::as_str),
        Some("agent-tui")
    );

    c.notify("notifications/initialized", json!({})).await?;
    c.shutdown().await?;
    std::fs::remove_dir_all(&root).ok();
    Ok(())
}

#[tokio::test]
async fn mcp_tools_list_includes_core_tools() -> Result<()> {
    let root = temp_root();
    let socket_dir = root.join("s");
    let state_home = root.join("x");
    std::fs::create_dir_all(&socket_dir)?;
    std::fs::create_dir_all(&state_home)?;

    let mut c = McpClient::start(&socket_dir, &state_home).await?;
    c.send(
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"},
        }),
    )
    .await?;
    c.notify("notifications/initialized", json!({})).await?;

    let tools = c.send("tools/list", json!({})).await?;
    let names: Vec<String> = tools
        .get("tools")
        .and_then(Value::as_array)
        .expect("tools array")
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    for required in ["spawn", "press", "snapshot", "wait", "die", "list"] {
        assert!(names.contains(&required.to_string()), "missing {required}");
    }

    c.shutdown().await?;
    std::fs::remove_dir_all(&root).ok();
    Ok(())
}

#[tokio::test]
async fn mcp_drives_real_pane_end_to_end() -> Result<()> {
    let root = temp_root();
    let socket_dir = root.join("s");
    let state_home = root.join("x");
    std::fs::create_dir_all(&socket_dir)?;
    std::fs::create_dir_all(&state_home)?;

    let mut c = McpClient::start(&socket_dir, &state_home).await?;
    c.send(
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"},
        }),
    )
    .await?;
    c.notify("notifications/initialized", json!({})).await?;

    // Spawn a pane that prints a known anchor.
    let spawn_result = c
        .send(
            "tools/call",
            json!({
                "name": "spawn",
                "arguments": {
                    "argv": ["bash", "-c", "echo MCP_HELLO_WORLD; sleep 60"]
                }
            }),
        )
        .await?;
    assert_eq!(
        spawn_result.get("isError").and_then(Value::as_bool),
        Some(false),
        "spawn returned error: {spawn_result}"
    );

    // Wait for the anchor text to appear in the pane.
    let wait_result = c
        .send(
            "tools/call",
            json!({
                "name": "wait",
                "arguments": { "text": "MCP_HELLO_WORLD", "max": 10000 }
            }),
        )
        .await?;
    assert_eq!(
        wait_result.get("isError").and_then(Value::as_bool),
        Some(false),
        "wait failed: {wait_result}"
    );

    // Snapshot the pane and inspect the wrapped envelope.
    let snapshot_result = c
        .send(
            "tools/call",
            json!({
                "name": "snapshot",
                "arguments": { "mode": "outline" }
            }),
        )
        .await?;
    let content = snapshot_result
        .get("content")
        .and_then(Value::as_array)
        .expect("content array");
    let text = content[0]
        .get("text")
        .and_then(Value::as_str)
        .expect("content text");
    // The text is the agent-tui response envelope as JSON.
    let envelope: Value = serde_json::from_str(text).expect("envelope JSON");
    let outline_str = serde_json::to_string(envelope.get("data").unwrap()).unwrap();
    assert!(
        outline_str.contains("MCP_HELLO_WORLD"),
        "snapshot outline missing anchor: {outline_str}"
    );

    // Close the pane.
    c.send(
        "tools/call",
        json!({
            "name": "die",
            "arguments": {}
        }),
    )
    .await?;

    c.shutdown().await?;
    // Best-effort: stop the daemon so the socket dir cleans up.
    let _ = std::process::Command::new(agent_tui_binary()?)
        .arg("--socket-dir")
        .arg(&socket_dir)
        .args(["daemon", "stop"])
        .env("XDG_STATE_HOME", &state_home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    std::fs::remove_dir_all(&root).ok();
    Ok(())
}

// Silence unused-import warnings under feature combos that don't pull
// in workspace_root.
#[allow(dead_code)]
fn _workspace_root_keepalive() -> std::path::PathBuf {
    workspace_root().unwrap()
}
