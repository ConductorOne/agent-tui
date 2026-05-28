//! The headline end-to-end scenario: MCP server → daemon → bwrap → vim.
//!
//! Proves every layer composes:
//!  1. MCP server reads JSON-RPC frames from stdio (Claude Desktop's view)
//!  2. → translates to agent-tui CLI commands
//!  3. → daemon (lazy-spawned) accepts spawn / press / snapshot calls
//!  4. → daemon spawns bwrap as the PTY child
//!  5. → bwrap sandboxes vim into the OCI rootfs
//!  6. → vim renders the seeded /fixtures/sample.txt
//!  7. → daemon's engine parses the PTY bytes
//!  8. → `VimAdapter` parses the outline (mode, file, statusline)
//!  9. → MCP server returns the structured outline as content
//!
//! Why this test exists: any layer regressing breaks the user's
//! experience. The vim-only bwrap tests don't exercise MCP. The MCP
//! smoke tests don't exercise bwrap+vim. This test covers the gap.

#![cfg(feature = "bwrap")]

use std::process::Stdio;
use std::time::Duration;

use agent_tui_integration::agent_tui_binary;
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
    #[allow(clippy::unused_async)]
    async fn start(socket_dir: &std::path::Path, state_home: &std::path::Path) -> Result<Self> {
        let bin = agent_tui_binary()?;
        let mut child = Command::new(&bin)
            .arg("--socket-dir")
            .arg(socket_dir)
            .args(["mcp", "serve"])
            .env("XDG_STATE_HOME", state_home)
            .env("AGENT_TUI_SOCKET_DIR", socket_dir)
            .env("AGENT_TUI_ALLOWED_BINARIES", "*")
            // Layer 1 cleanup: daemon dies with this test process.
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
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let s = serde_json::to_string(&frame)?;
        self.stdin.write_all(s.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
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
        }
    }

    async fn notify(&mut self, method: &str) -> Result<()> {
        let frame = json!({"jsonrpc": "2.0", "method": method, "params": {}});
        let s = serde_json::to_string(&frame)?;
        self.stdin.write_all(s.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn call(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let result = self
            .send("tools/call", json!({"name": name, "arguments": arguments}))
            .await?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|c| c.get("text"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing content[0].text for {name}"))?;
        let envelope: Value = serde_json::from_str(text)?;
        if is_error {
            return Err(anyhow!("tool {name} failed: {envelope}"));
        }
        Ok(envelope)
    }

    async fn shutdown(mut self) {
        drop(self.stdin);
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
    }
}

fn vim_rootfs() -> Result<std::path::PathBuf> {
    let root = agent_tui_integration::workspace_root()?
        .join("target")
        .join("integration-rootfs")
        .join("vim")
        .join("extracted");
    if !root.join("usr").exists() {
        anyhow::bail!("vim rootfs not built; run `just rootfs vim`");
    }
    Ok(root)
}

fn bwrap_vim_argv(rootfs: &std::path::Path, scratch: &std::path::Path) -> Vec<String> {
    [
        "bwrap",
        "--ro-bind",
        &rootfs.to_string_lossy(),
        "/",
        "--ro-bind",
        "/proc",
        "/proc",
        "--dev-bind",
        "/dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--tmpfs",
        "/var/tmp",
        "--tmpfs",
        "/run",
        "--tmpfs",
        "/home",
        "--tmpfs",
        "/root",
        "--bind",
        &scratch.to_string_lossy(),
        "/work",
        "--setenv",
        "HOME",
        "/root",
        "--setenv",
        "TERM",
        "xterm-256color",
        "--setenv",
        "PATH",
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        "--unshare-user",
        "--unshare-net",
        "--unshare-ipc",
        "--unshare-uts",
        "--die-with-parent",
        "--",
        "vim",
        "/fixtures/sample.txt",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

#[tokio::test]
async fn mcp_drives_vim_through_bwrap_end_to_end() -> Result<()> {
    let rootfs = vim_rootfs()?;
    let nonce: String = uuid::Uuid::new_v4().to_string().chars().take(8).collect();
    let root = std::env::temp_dir().join(format!("at-mcp-vim-{nonce}"));
    let socket_dir = root.join("s");
    let state_home = root.join("x");
    let scratch = root.join("w");
    for d in [&socket_dir, &state_home, &scratch] {
        std::fs::create_dir_all(d)?;
    }

    let mut c = McpClient::start(&socket_dir, &state_home).await?;
    c.send(
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "agent-tui-headline-test", "version": "0.1.0"},
        }),
    )
    .await?;
    c.notify("notifications/initialized").await?;

    // Step 1: Claude calls spawn — we wrap the argv with bwrap so the
    // child is sandboxed. (In a real Claude Desktop session the user
    // would do this via a "drive vim under bwrap" tool, or the harness
    // could be hidden in a script the agent calls.)
    let argv = bwrap_vim_argv(&rootfs, &scratch);
    c.call("spawn", json!({ "argv": argv })).await?;

    // Step 2: Claude waits for vim to render the file.
    c.call("wait", json!({ "text": "sample\\.txt", "max": 10000 }))
        .await?;
    c.call("wait", json!({ "idle": 150, "max": 5000 })).await?;

    // Step 3: Claude takes a snapshot to see what's in vim. This is the
    // POINT of the integration — VimAdapter MUST produce the structured
    // outline (mode=normal, file=/fixtures/sample.txt) all the way back
    // through MCP.
    let snap = c.call("snapshot", json!({ "mode": "outline" })).await?;
    let outline = snap.get("data").and_then(|d| d.get("outline")).unwrap();
    assert_eq!(
        outline.get("adapter").and_then(Value::as_str),
        Some("vim"),
        "MCP didn't surface vim adapter; outline = {outline}"
    );
    // Under the addressing model the vim adapter emits a single `@vim`
    // root with the mode/file/buffer nodes as children. Walk
    // recursively to find role-tagged nodes wherever they live.
    fn find_role<'a>(outline: &'a Value, role: &str) -> Option<&'a Value> {
        fn walk<'a>(n: &'a Value, want: &str) -> Option<&'a Value> {
            if n.get("role").and_then(Value::as_str) == Some(want) {
                return Some(n);
            }
            n.get("children")
                .and_then(Value::as_array)
                .and_then(|kids| kids.iter().find_map(|k| walk(k, want)))
        }
        outline
            .get("nodes")
            .and_then(Value::as_array)?
            .iter()
            .find_map(|n| walk(n, role))
    }
    let mode_node = find_role(outline, "mode").expect("mode node");
    // The mode adapter stores the mode name in `value` (the
    // semantic payload of the indicator); `name` is empty.
    assert_eq!(
        mode_node.get("value").and_then(Value::as_str),
        Some("normal")
    );
    let file_node = find_role(outline, "file").expect("file node");
    assert_eq!(
        file_node.get("name").and_then(Value::as_str),
        Some("/fixtures/sample.txt")
    );

    // Step 4: Claude presses `i` to enter insert mode. Use the new
    // ref-based wait — selectors don't false-fire the way
    // `wait text=INSERT` might (the marker shows up briefly while
    // vim repaints the modeline).
    c.call("press", json!({ "keys": "i" })).await?;
    c.call(
        "wait",
        json!({ "ref": "@vim.mode[value=insert]", "max": 5000 }),
    )
    .await?;

    // Step 5: Snapshot again — Claude sees mode=insert in the outline.
    let snap = c.call("snapshot", json!({ "mode": "outline" })).await?;
    let outline = snap.get("data").and_then(|d| d.get("outline")).unwrap();
    let mode_node = find_role(outline, "mode").expect("mode node");
    assert_eq!(
        mode_node.get("value").and_then(Value::as_str),
        Some("insert"),
        "expected mode=insert after `i`; outline = {outline}"
    );

    // Step 6: Clean shutdown.
    c.call("press", json!({ "keys": "<esc>:q!<cr>" })).await?;
    c.call("die", json!({})).await?;
    c.shutdown().await;

    // Stop the daemon so the next test gets a fresh socket dir.
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
