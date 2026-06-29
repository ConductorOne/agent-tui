//! MCP (Model Context Protocol) server mode.
//!
//! `agent-tui mcp serve` exposes the CLI surface as MCP tools over
//! line-delimited JSON-RPC on stdio. Claude Desktop / Claude Code /
//! any MCP client can drive panes through it: spawn vim, press keys,
//! wait for text, snapshot the outline.
//!
//! Wire-level details (RFC §13.4):
//!  - Transport: stdio, one JSON-RPC message per line (NDJSON), no
//!    `Content-Length` headers.
//!  - JSON-RPC 2.0: `{"jsonrpc": "2.0", "id": <id>, "method": ..., "params": ...}`
//!  - Required handshake: `initialize` → response with server capabilities,
//!    followed by client's `notifications/initialized` notification.
//!  - Tool dispatch: `tools/list` returns the schemas; `tools/call` runs
//!    one tool and returns a content envelope.
//!
//! Each tool is a thin wrapper around the existing agent-tui CLI
//! command path — the MCP layer parses the MCP envelope, builds the
//! corresponding `Command`, dispatches via the daemon client, and
//! returns the response envelope as the tool's result text. The
//! daemon's lazy-spawn machinery is transparent: the first tool call
//! that needs a daemon starts one.
//!
//! What's NOT implemented yet: streaming progress (the long-running
//! `wait` tool can take seconds); rich content types beyond `text`;
//! resource listings (`resources/list`); prompt templates
//! (`prompts/list`). The current shape covers the "agent drives a
//! pane" use case end-to-end.

use std::io::Write;

use agent_tui_daemon::SocketLayout;
use agent_tui_protocol::{Command, PaneId};
use anyhow::Result;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, BufReader};

use crate::cli::GlobalArgs;
use crate::client;

/// MCP protocol version we advertise in `initialize`. Matches the
/// 2024-11-05 spec — broad enough that current Claude clients accept it.
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Maximum bytes accepted for one MCP JSON-RPC line, including newline.
const MAX_MCP_LINE_BYTES: usize = 1024 * 1024;

/// Run the MCP server loop on stdio until EOF on stdin.
pub async fn serve(globals: GlobalArgs) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let layout = client::layout_for(&globals.session, globals.socket_dir.as_deref());

    loop {
        let line = match read_mcp_line(&mut reader).await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(e) => {
                send_response(&parse_error(None, &format!("invalid MCP frame: {e}")))?;
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                send_response(&parse_error(None, &format!("invalid JSON: {e}")))?;
                continue;
            }
        };

        // Notifications have no `id`; we don't respond to them.
        let id = request.get("id").cloned();
        let is_notification = id.is_none();

        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        let response = dispatch(&globals, &layout, method, params).await;

        if is_notification {
            // No reply for notifications.
            continue;
        }
        match response {
            Ok(result) => send_response(&success(id, &result))?,
            Err(err) => send_response(&error(id, &err))?,
        }
    }
    Ok(())
}

async fn read_mcp_line<R>(reader: &mut R) -> std::io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    let mut buf = Vec::new();
    let read = {
        let mut limited = (&mut *reader).take((MAX_MCP_LINE_BYTES + 1) as u64);
        limited.read_until(b'\n', &mut buf).await?
    };
    if read == 0 && buf.is_empty() {
        return Ok(None);
    }
    if buf.len() > MAX_MCP_LINE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("MCP frame exceeds {MAX_MCP_LINE_BYTES} bytes"),
        ));
    }
    String::from_utf8(buf).map(Some).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("MCP frame is not valid UTF-8: {e}"),
        )
    })
}

/// Dispatch one method. Returns the `result` field on success, or an
/// `(error_code, message)` pair on failure.
async fn dispatch(
    globals: &GlobalArgs,
    layout: &SocketLayout,
    method: &str,
    params: Value,
) -> Result<Value, McpError> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "agent-tui",
                "version": env!("CARGO_PKG_VERSION"),
            }
        })),

        // Notifications. No-op except for confirming the server saw them.
        "notifications/initialized" | "notifications/cancelled" => Ok(Value::Null),

        "tools/list" => Ok(json!({ "tools": tool_schemas() })),

        "tools/call" => call_tool(globals, layout, params).await,

        // RFC says servers MAY support resources / prompts; we don't.
        // The MCP spec requires servers to return a method-not-found error
        // for unknown methods (-32601 by JSON-RPC convention).
        _ => Err(McpError {
            code: -32601,
            message: format!("method not found: {method}"),
        }),
    }
}

/// Translate a `tools/call` payload into an agent-tui Command, dispatch
/// it via the daemon client, and wrap the response envelope in MCP
/// `content` format.
async fn call_tool(
    globals: &GlobalArgs,
    layout: &SocketLayout,
    params: Value,
) -> Result<Value, McpError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::invalid_params("tools/call requires `name`"))?;
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    let command = build_command(name, &args).map_err(McpError::invalid_params)?;
    let lazy_spawn =
        client::LazySpawnConfig::from_allowed_binaries(globals.allowed_binaries.as_deref());
    let envelope = client::one_shot_with_config(layout, command, &lazy_spawn)
        .await
        .map_err(|e| McpError::internal(&e.to_string()))?;

    // Convention: wrap the agent-tui envelope as ONE text content entry.
    // Agents see a single JSON blob they can serde-parse — same shape as
    // `agent-tui --json` returns. We also surface `isError` so MCP
    // clients can short-circuit error paths without re-parsing.
    let text = serde_json::to_string(&envelope)
        .map_err(|e| McpError::internal(&format!("serialize response: {e}")))?;
    let is_error = envelope.response.is_failure();
    Ok(json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": is_error,
    }))
}

/// Map a tool name + JSON args object to an agent-tui `Command`.
#[allow(clippy::too_many_lines, clippy::similar_names)]
fn build_command(name: &str, args: &Value) -> Result<Command, String> {
    let obj = args
        .as_object()
        .ok_or_else(|| "arguments must be an object".to_string())?;

    let pane_id = obj
        .get("pane")
        .and_then(Value::as_str)
        .map(|s| PaneId(s.to_string()));

    match name {
        "spawn" => {
            let argv = obj
                .get("argv")
                .and_then(Value::as_array)
                .ok_or_else(|| "spawn requires `argv` array".to_string())?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| "argv items must be strings".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let cwd = obj.get("cwd").and_then(Value::as_str).map(str::to_string);
            let size = obj.get("size").and_then(Value::as_array).and_then(|a| {
                if a.len() == 2 {
                    let c = u16::try_from(a[0].as_u64()?).ok()?;
                    let r = u16::try_from(a[1].as_u64()?).ok()?;
                    Some((c, r))
                } else {
                    None
                }
            });
            Ok(Command::Spawn {
                argv,
                cwd,
                size,
                stdin: agent_tui_protocol::request::StdinMode::default(),
                env: Vec::new(),
            })
        }
        "list" => Ok(Command::List {
            all: obj.get("all").and_then(Value::as_bool).unwrap_or(false),
        }),
        "snapshot" => {
            let mode = obj.get("mode").and_then(Value::as_str).unwrap_or("outline");
            let mode = mode_from_str(mode)?;
            let png = obj.get("png").and_then(Value::as_str).map(str::to_string);
            // `annotate` accepts a boolean (true = annotate all refs) or a
            // selector string (annotate only matching refs); absent = off.
            let annotate = match obj.get("annotate") {
                Some(Value::Bool(true)) => Some(String::new()),
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            };
            // `chrome` accepts a boolean (true = frame, OSC/no title) or a
            // string (the title-bar text); absent = bare grid.
            let chrome = match obj.get("chrome") {
                Some(Value::Bool(true)) => Some(String::new()),
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            };
            let select = obj
                .get("select")
                .and_then(Value::as_str)
                .map(str::to_string);
            let all = obj.get("all").and_then(Value::as_bool).unwrap_or(false);
            let keep_color = obj
                .get("keep_color")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(Command::Snapshot {
                pane: pane_id,
                mode,
                png,
                annotate,
                chrome,
                select,
                all,
                keep_color,
            })
        }
        "press" => {
            let keys = obj
                .get("keys")
                .and_then(Value::as_str)
                .ok_or_else(|| "press requires `keys` string".to_string())?
                .to_string();
            let to = obj.get("to").and_then(Value::as_str).map(str::to_string);
            Ok(Command::Press {
                pane: pane_id,
                keys,
                to,
                lease: None,
            })
        }
        "type" => {
            let text = obj
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| "type requires `text` string".to_string())?
                .to_string();
            let to = obj.get("to").and_then(Value::as_str).map(str::to_string);
            Ok(Command::Type {
                pane: pane_id,
                text,
                to,
                lease: None,
            })
        }
        "wait" => {
            use agent_tui_protocol::request::WaitCondition;
            let condition = if let Some(seq) = obj.get("since").and_then(Value::as_u64) {
                WaitCondition::Since { since: seq }
            } else if let Some(h) = obj.get("hash").and_then(Value::as_str) {
                WaitCondition::Hash {
                    hash: h.to_string(),
                }
            } else if let Some(ms) = obj.get("idle").and_then(Value::as_u64) {
                WaitCondition::Idle { quiet_ms: ms }
            } else if let Some(r) = obj.get("text").and_then(Value::as_str) {
                WaitCondition::Text {
                    regex: r.to_string(),
                }
            } else if let Some(ms) = obj.get("cursor_stable").and_then(Value::as_u64) {
                WaitCondition::CursorStable { stable_ms: ms }
            } else if let Some(on) = obj.get("alt_screen").and_then(Value::as_bool) {
                WaitCondition::AltScreen { on }
            } else if obj.get("exit").and_then(Value::as_bool).unwrap_or(false) {
                WaitCondition::Exit
            } else if let Some(sel) = obj.get("ref").and_then(Value::as_str) {
                WaitCondition::Ref {
                    selector: sel.to_string(),
                    gone: obj.get("gone").and_then(Value::as_bool).unwrap_or(false),
                }
            } else {
                return Err(
                    "wait requires one of: since/hash/idle/text/cursor_stable/alt_screen/exit/ref"
                        .to_string(),
                );
            };
            let max = obj.get("max").and_then(Value::as_u64).unwrap_or(10000);
            Ok(Command::Wait {
                pane: pane_id,
                condition,
                timeout: std::time::Duration::from_millis(max),
            })
        }
        "die" => {
            let grace = obj
                .get("grace")
                .and_then(Value::as_u64)
                .map(std::time::Duration::from_millis);
            Ok(Command::Die {
                pane: pane_id,
                grace,
            })
        }
        "focus" => {
            let target = obj
                .get("pane")
                .and_then(Value::as_str)
                .ok_or_else(|| "focus requires `pane` string (or \"none\")".to_string())?;
            let pane = if target.eq_ignore_ascii_case("none") {
                None
            } else {
                Some(PaneId(target.to_string()))
            };
            Ok(Command::Focus { pane })
        }
        "daemon_status" => Ok(Command::DaemonStatus),
        _ => Err(format!("unknown tool: {name}")),
    }
}

fn mode_from_str(mode: &str) -> Result<agent_tui_protocol::request::SnapshotMode, String> {
    use agent_tui_protocol::request::SnapshotMode;
    match mode {
        "outline" => Ok(SnapshotMode::Outline),
        "text" => Ok(SnapshotMode::Text),
        "cells" => Ok(SnapshotMode::Cells),
        "adapter" => Ok(SnapshotMode::Adapter),
        "hybrid" => Ok(SnapshotMode::Hybrid),
        other => Err(format!(
            "unknown snapshot mode {other:?}; valid: outline|text|cells|adapter|hybrid"
        )),
    }
}

/// Tool schemas surface in `tools/list` responses. Each schema is JSON
/// Schema (draft 7-ish) — same conventions Claude uses elsewhere.
#[allow(clippy::too_many_lines)]
fn tool_schemas() -> Vec<Value> {
    vec![
        json!({
            "name": "spawn",
            "description": "Start a PTY-backed pane running the given argv. The pane becomes the focused target for subsequent press / type / snapshot calls.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "argv": { "type": "array", "items": {"type": "string"}, "minItems": 1 },
                    "cwd": { "type": "string" },
                    "size": { "type": "array", "items": {"type": "integer"}, "minItems": 2, "maxItems": 2 }
                },
                "required": ["argv"]
            }
        }),
        json!({
            "name": "list",
            "description": "List panes in the current session (or all sessions with `all: true`).",
            "inputSchema": {
                "type": "object",
                "properties": { "all": { "type": "boolean" } }
            }
        }),
        json!({
            "name": "snapshot",
            "description": "Capture the focused pane (or a named one). `mode` selects the payload shape: 'outline' (structured per-adapter view, default), 'text' (visible cells flattened to a plain UTF-8 string — the right answer for 'what does the screen say' on unstructured output), 'cells' (raw RLE-packed grid), 'adapter' (just the adapter outline), 'hybrid' (outline + cells + adapter + text). Optional `select` is a CSS-subset selector that filters the outline to matching nodes only (e.g. `[role=buffer][focused]`, `@vim.statusline`); `all` returns every match in depth-first pre-order. With `select`, an outline is always included even if `mode` would otherwise omit it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": { "type": "string" },
                    "mode": {
                        "type": "string",
                        "enum": ["outline", "text", "cells", "adapter", "hybrid"]
                    },
                    "png": { "type": "string", "description": "Path to write a real PNG render of the screen (monospace glyphs + cell colors)." },
                    "annotate": { "type": ["boolean", "string"], "description": "With `png`, overlay ref bounding boxes + numbered badges. `true` annotates all refs; a selector string annotates only matching refs (e.g. `@vim.*`). Requires `png`." },
                    "chrome": { "type": ["boolean", "string"], "description": "With `png`, composite a marketing-grade window frame (padding, rounded corners, title bar + traffic lights, drop shadow, brand-dark backdrop). `true` frames with the terminal's own title; a string sets the title-bar text. Default off. Requires `png`." },
                    "select": { "type": "string", "description": "CSS-subset selector. See docs/design/addressing-rfc.md §2.2." },
                    "all": { "type": "boolean", "description": "With `select`, return every match instead of just the first." }
                }
            }
        }),
        json!({
            "name": "press",
            "description": "Send a key-token sequence to the focused pane (or a named pane). Bash-style angle-bracket syntax: 'i hello<esc>:w<cr>'. Supports <cr>, <esc>, <c-X>, <a-X>, <f1>..<f12>, <up>/<down>/<left>/<right>, <home>, <end>, <pageup>, <pagedown>, <del>, <ins>, <tab>, <bs>, <space>, <lf>. Escape a literal `<` as <\\\\<>. Optional `to` is a selector identifying a sub-node target (e.g. `@tmux.pane[%2]`); the pane's adapter translates `(target, keys)` into bytes (RFC §2.3). Without `to`, the keys go straight to the PTY.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": { "type": "string" },
                    "keys": { "type": "string" },
                    "to": { "type": "string", "description": "Selector identifying the target node. See docs/design/addressing-rfc.md §2.3." }
                },
                "required": ["keys"]
            }
        }),
        json!({
            "name": "type",
            "description": "Type literal UTF-8 text to the focused pane. Use `press` for control keys; `type` does no key-token interpretation. Optional `to` is a selector identifying a sub-node target; the adapter translates the routing (RFC §2.3).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": { "type": "string" },
                    "text": { "type": "string" },
                    "to": { "type": "string" }
                },
                "required": ["text"]
            }
        }),
        json!({
            "name": "wait",
            "description": "Block until a pane state change. Exactly one of: `since` (sequence past), `hash` (grid hash equals), `idle` (no output for ms), `text` (regex appears), `cursor_stable` (cursor unchanged for ms), `alt_screen` (alt-screen on/off), `exit` (child process exits), `ref` (a selector matches a node in the outline; combine with `gone:true` to wait for it to stop matching — useful for 'wait for the confirm to dismiss'). `max` caps the wall-clock timeout (default 10000ms). Prefer `ref` over `text` when the adapter exposes structured nodes — selectors don't false-fire on typed-but-not-yet-executed text.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": { "type": "string" },
                    "since": { "type": "integer" },
                    "hash": { "type": "string" },
                    "idle": { "type": "integer" },
                    "text": { "type": "string" },
                    "cursor_stable": { "type": "integer" },
                    "alt_screen": { "type": "boolean" },
                    "exit": { "type": "boolean" },
                    "ref": { "type": "string", "description": "CSS-subset selector. See docs/design/addressing-rfc.md §2.2." },
                    "gone": { "type": "boolean", "description": "With `ref`, fires when no node matches." },
                    "max": { "type": "integer" }
                }
            }
        }),
        json!({
            "name": "die",
            "description": "Close a pane with group-aware teardown (so the harness's forked subprocesses aren't orphaned). With `grace`, SIGTERM the process group, wait up to `grace` ms, then SIGKILL the group; without it, send a single immediate SIGTERM to the group.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": { "type": "string" },
                    "grace": { "type": "integer", "description": "Graceful teardown window in ms: SIGTERM group → wait → SIGKILL group. Omit for an immediate group SIGTERM." }
                }
            }
        }),
        json!({
            "name": "focus",
            "description": "Set the focused pane for the session. Use `pane: \"none\"` to clear focus.",
            "inputSchema": {
                "type": "object",
                "properties": { "pane": { "type": "string" } },
                "required": ["pane"]
            }
        }),
        json!({
            "name": "daemon_status",
            "description": "Query daemon health, version, uptime, and pane count. Useful as a heartbeat before starting work.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
    ]
}

#[derive(Debug)]
struct McpError {
    code: i64,
    message: String,
}

impl McpError {
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }
    fn internal(message: &str) -> Self {
        Self {
            code: -32603,
            message: message.to_string(),
        }
    }
}

fn success(id: Option<Value>, result: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result,
    })
}

fn error(id: Option<Value>, err: &McpError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": { "code": err.code, "message": err.message },
    })
}

fn parse_error(id: Option<Value>, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": { "code": -32700, "message": message },
    })
}

/// Write one JSON object + trailing newline to stdout. The MCP spec
/// says one JSON value per line; we don't pretty-print so the framing
/// is unambiguous.
fn send_response(value: &Value) -> Result<()> {
    let line = serde_json::to_string(value)?;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    handle.write_all(line.as_bytes())?;
    handle.write_all(b"\n")?;
    handle.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_tui_protocol::request::SnapshotMode;

    #[tokio::test]
    async fn read_mcp_line_accepts_normal_line() {
        let mut reader = BufReader::new(std::io::Cursor::new(b"{\"jsonrpc\":\"2.0\"}\n".to_vec()));
        let line = read_mcp_line(&mut reader)
            .await
            .expect("read MCP line")
            .expect("line");
        assert_eq!(line, "{\"jsonrpc\":\"2.0\"}\n");
    }

    #[tokio::test]
    async fn read_mcp_line_rejects_oversized_frame() {
        let mut reader = BufReader::new(std::io::Cursor::new(vec![b'a'; MAX_MCP_LINE_BYTES + 1]));
        let err = read_mcp_line(&mut reader)
            .await
            .expect_err("oversized MCP frame must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn build_command_spawn() {
        let args = json!({ "argv": ["bash", "-l"] });
        let cmd = build_command("spawn", &args).unwrap();
        match cmd {
            Command::Spawn { argv, .. } => assert_eq!(argv, vec!["bash", "-l"]),
            _ => panic!("expected Spawn"),
        }
    }

    #[test]
    fn build_command_press_requires_keys() {
        let args = json!({});
        let err = build_command("press", &args).unwrap_err();
        assert!(err.contains("keys"), "{err}");
    }

    #[test]
    fn build_command_snapshot_defaults_to_outline_mode() {
        let args = json!({});
        let cmd = build_command("snapshot", &args).unwrap();
        match cmd {
            Command::Snapshot { mode, .. } => assert!(matches!(mode, SnapshotMode::Outline)),
            _ => panic!("expected Snapshot"),
        }
    }

    #[test]
    fn build_command_snapshot_accepts_text_mode() {
        // Regression for PR-E1 / RFC §P0-1: `mode:"text"` used to be
        // rejected over MCP even though the CLI + engine support it.
        let args = json!({ "mode": "text" });
        let cmd = build_command("snapshot", &args).unwrap();
        match cmd {
            Command::Snapshot { mode, .. } => assert!(matches!(mode, SnapshotMode::Text)),
            _ => panic!("expected Snapshot"),
        }
    }

    #[test]
    fn snapshot_schema_advertises_text_mode() {
        let schema = tool_schemas()
            .into_iter()
            .find(|t| t["name"] == "snapshot")
            .expect("snapshot schema");
        let modes: Vec<String> = schema["inputSchema"]["properties"]["mode"]["enum"]
            .as_array()
            .expect("mode enum")
            .iter()
            .map(|m| m.as_str().unwrap().to_string())
            .collect();
        assert!(modes.contains(&"text".to_string()), "modes: {modes:?}");
    }

    #[test]
    fn build_command_snapshot_rejects_unknown_mode_with_valid_set() {
        let args = json!({ "mode": "bogus" });
        let err = build_command("snapshot", &args).unwrap_err();
        assert!(
            err.contains("text"),
            "error should name the valid set: {err}"
        );
    }

    #[test]
    fn build_command_wait_picks_first_matching_condition() {
        use agent_tui_protocol::request::WaitCondition;
        let args = json!({ "text": "ready" });
        let cmd = build_command("wait", &args).unwrap();
        match cmd {
            Command::Wait { condition, .. } => match condition {
                WaitCondition::Text { regex } => assert_eq!(regex, "ready"),
                other => panic!("expected Text wait, got {other:?}"),
            },
            _ => panic!("expected Wait"),
        }
    }

    #[test]
    fn build_command_focus_handles_none_keyword() {
        let args = json!({ "pane": "none" });
        let cmd = build_command("focus", &args).unwrap();
        match cmd {
            Command::Focus { pane } => assert!(pane.is_none()),
            _ => panic!("expected Focus"),
        }
    }

    #[test]
    fn build_command_unknown_returns_error() {
        let err = build_command("bogus", &json!({})).unwrap_err();
        assert!(err.contains("unknown tool"), "{err}");
    }

    #[test]
    fn tool_schemas_cover_every_dispatch_branch() {
        // Sanity: every name in build_command's match must appear in the
        // tools/list response, and vice-versa.
        let names: std::collections::BTreeSet<String> = tool_schemas()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        let expected: std::collections::BTreeSet<&str> = [
            "spawn",
            "list",
            "snapshot",
            "press",
            "type",
            "wait",
            "die",
            "focus",
            "daemon_status",
        ]
        .into_iter()
        .collect();
        for name in &expected {
            assert!(names.contains(*name), "missing schema for {name}");
        }
    }
}
