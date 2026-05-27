//! Fake AI-inference HTTP server for integration tests.
//!
//! Stands up a localhost TCP server that mimics the `OpenAI` Chat
//! Completions API (the protocol most "openai-compatible" clients
//! use, including `OpenCode` and `Pi` via their custom-provider config).
//! Each scenario configures a [`Script`] that decides what the server
//! replies with — exact tokens, stream chunk boundaries, optional
//! per-chunk latency. The agent CLI under test then points at our
//! `baseURL` and renders the canned response in real PTY traffic.
//!
//! Why hand-rolled HTTP/1.1 instead of axum/hyper:
//!  - Zero added workspace deps. Tokio is already in.
//!  - The server only handles one endpoint with a fixed request shape.
//!    The parsing surface is ~30 lines.
//!  - Cuts compile-time cost; this is dev/test code.
//!
//! Design notes:
//!  - Binds 127.0.0.1:0 so the OS assigns a free port. Tests read it
//!    back via [`FakeServer::url`] and pass to the agent CLI via env
//!    or config file.
//!  - SSE chunks are written one at a time with optional `delay_ms`
//!    so we can test the agent's streaming-render path under realistic
//!    latency.
//!  - Each scenario gets its own server instance; concurrency is
//!    sequential per server because that's how chat APIs work.
//!  - Shuts down cleanly on `Drop` (broadcast channel kills the
//!    accept loop).

#![allow(clippy::missing_errors_doc)]

use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

/// One server-side reply slot: either a sequence of text chunks OR a
/// function/tool call. The Script DSL chooses which.
#[derive(Debug, Clone, Default)]
pub struct Reply {
    /// Successive text chunks that get streamed as
    /// `delta.content` SSE events. Empty when this reply is a tool call.
    pub chunks: Vec<String>,
    /// When present, the reply emits a function-call event sequence
    /// (Responses API: `output_item.added(type=function_call)`,
    /// `function_call_arguments.delta×N`, etc.) instead of text deltas.
    /// Only honored by the Responses-API writer; the chat-completions
    /// writer falls back to an empty text reply.
    pub tool_call: Option<ToolCall>,
    /// Optional per-chunk latency. Defaults to 5ms if unset.
    pub delay_ms: u64,
    /// `finish_reason` in the final SSE chunk. Default `"stop"`.
    pub finish_reason: String,
}

/// A single tool/function call the assistant is asking the client to
/// execute. Shape matches the `OpenAI` Responses API
/// `output_item(type=function_call)`.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Tool name as registered by the client (e.g. `"bash"` in OpenCode).
    pub name: String,
    /// JSON-encoded arguments string passed to the tool. We stream this
    /// as one `function_call_arguments.delta` event for simplicity —
    /// real models stream it character-by-character but clients accept
    /// either pattern.
    pub arguments: String,
    /// `call_id` echoed back by the client in the follow-up request's
    /// `function_call_output` item. Auto-generated when constructed via
    /// [`Reply::tool_call`].
    pub call_id: String,
}

impl Reply {
    /// Whole reply as one chunk, no delay.
    pub fn one(text: impl Into<String>) -> Self {
        Self {
            chunks: vec![text.into()],
            tool_call: None,
            delay_ms: 0,
            finish_reason: "stop".into(),
        }
    }

    /// Streamed reply split into the given chunks.
    pub fn streamed<I, S>(chunks: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            chunks: chunks.into_iter().map(Into::into).collect(),
            tool_call: None,
            delay_ms: 5,
            finish_reason: "stop".into(),
        }
    }

    /// A reply that asks the client to invoke a tool. `arguments` is a
    /// JSON string the client passes to the tool's handler (e.g.
    /// `{"command":"echo hi"}` for OpenCode's `bash` tool).
    pub fn tool_call(name: impl Into<String>, arguments: impl Into<String>) -> Self {
        Self {
            chunks: Vec::new(),
            tool_call: Some(ToolCall {
                name: name.into(),
                arguments: arguments.into(),
                call_id: format!("call_{}", short_id()),
            }),
            delay_ms: 0,
            finish_reason: "tool_calls".into(),
        }
    }

    /// Set per-chunk latency (useful for testing spinner cadence).
    #[must_use]
    pub fn with_delay(mut self, ms: u64) -> Self {
        self.delay_ms = ms;
        self
    }
}

/// Script the fake server walks through as requests arrive.
///
/// The N-th request gets the N-th reply. Past the end of the script,
/// the server returns the last reply on a loop (defensive: real CLIs
/// sometimes retry once after a non-2xx).
#[derive(Debug, Clone, Default)]
pub struct Script {
    replies: Vec<Reply>,
}

impl Script {
    /// Empty script (will return 500 on every request — useful for
    /// "agent should handle errors gracefully" scenarios).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a reply.
    #[must_use]
    pub fn reply(mut self, r: Reply) -> Self {
        self.replies.push(r);
        self
    }

    /// Convenience: one canned text response.
    #[must_use]
    pub fn say(self, text: impl Into<String>) -> Self {
        self.reply(Reply::one(text))
    }

    /// Convenience: a streamed response with explicit chunks.
    #[must_use]
    pub fn stream<I, S>(self, chunks: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.reply(Reply::streamed(chunks))
    }

    /// Convenience: a tool/function call response. `arguments` is a
    /// JSON string the client passes to the tool's handler.
    #[must_use]
    pub fn tool_call(self, name: impl Into<String>, arguments: impl Into<String>) -> Self {
        self.reply(Reply::tool_call(name, arguments))
    }
}

/// One observed inbound request — captured so tests can assert "the
/// agent under test actually hit our endpoint" without needing to peek
/// at the agent's own logs (which are often hidden under bwrap).
#[derive(Debug, Clone)]
pub struct ReceivedRequest {
    /// Request line, e.g. `POST /v1/responses HTTP/1.1`.
    pub request_line: String,
    /// Parsed path component, e.g. `/v1/responses`.
    pub path: String,
    /// Decoded body JSON if Content-Length > 0 and parse succeeded;
    /// `Value::Null` otherwise.
    pub body: Value,
}

/// Running fake-inference server.
pub struct FakeServer {
    addr: SocketAddr,
    shutdown_tx: broadcast::Sender<()>,
    requests: Arc<Mutex<Vec<ReceivedRequest>>>,
    _accept_task: tokio::task::JoinHandle<()>,
}

impl FakeServer {
    /// Bind on `127.0.0.1:0`, start accepting, and return.
    pub async fn start(script: Script) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind fake-inference listener")?;
        let addr = listener.local_addr()?;
        let (shutdown_tx, _) = broadcast::channel(1);
        let script = Arc::new(Mutex::new(ScriptState {
            replies: script.replies,
            cursor: 0,
        }));
        let requests: Arc<Mutex<Vec<ReceivedRequest>>> = Arc::new(Mutex::new(Vec::new()));

        let shutdown = shutdown_tx.clone();
        let req_log = requests.clone();
        let accept = tokio::spawn(async move {
            loop {
                let mut shutdown_rx = shutdown.subscribe();
                tokio::select! {
                    _ = shutdown_rx.recv() => break,
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((stream, _peer)) => {
                                let script = script.clone();
                                let req_log = req_log.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = handle_connection(stream, script, req_log).await {
                                        tracing::debug!(error = %e, "fake-inference conn error");
                                    }
                                });
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "fake-inference accept error");
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(Self {
            addr,
            shutdown_tx,
            requests,
            _accept_task: accept,
        })
    }

    /// HTTP base URL agent CLIs should point at, e.g.
    /// `http://127.0.0.1:43219`.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Endpoint URL for the `OpenAI` Chat Completions API.
    #[must_use]
    pub fn openai_v1_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    /// Snapshot of every request the server has received, in order.
    ///
    /// Use in test assertions like `assert!(server.requests().iter()
    /// .any(|r| r.path == "/v1/responses"))` to prove the agent under
    /// test reached the fake endpoint — distinguishes "agent didn't
    /// call us" from "agent called but parsed our reply wrong."
    #[must_use]
    pub fn requests(&self) -> Vec<ReceivedRequest> {
        self.requests
            .lock()
            .expect("fake-server requests mutex poisoned")
            .clone()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(());
    }
}

struct ScriptState {
    replies: Vec<Reply>,
    cursor: usize,
}

impl ScriptState {
    fn next(&mut self) -> Option<Reply> {
        if self.replies.is_empty() {
            return None;
        }
        let i = self.cursor.min(self.replies.len() - 1);
        self.cursor += 1;
        Some(self.replies[i].clone())
    }
}

async fn handle_connection(
    stream: TcpStream,
    script: Arc<Mutex<ScriptState>>,
    req_log: Arc<Mutex<Vec<ReceivedRequest>>>,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Minimal HTTP/1.1 request parser: read request line, then headers
    // until empty line, then body if Content-Length present.
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    let request_line = request_line.trim().to_string();

    let mut content_length: usize = 0;
    let mut want_sse = false;
    loop {
        let mut header = String::new();
        let n = reader.read_line(&mut header).await?;
        if n == 0 || header == "\r\n" || header == "\n" {
            break;
        }
        let lower = header.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("accept:") && lower.contains("text/event-stream") {
            want_sse = true;
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).await?;
    }
    let body_json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

    // Log the request before dispatch so a test can see what hit us
    // even if our reply-writing path errors out.
    {
        let path_str = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or("")
            .to_string();
        req_log
            .lock()
            .expect("fake-server requests mutex poisoned")
            .push(ReceivedRequest {
                request_line: request_line.clone(),
                path: path_str,
                body: body_json.clone(),
            });
    }
    // `stream=true` in the JSON body overrides the Accept header.
    let stream_requested = want_sse
        || body_json
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);

    let Some(reply) = script.lock().expect("script mutex poisoned").next() else {
        write_status(&mut write_half, 500, "no script entry available").await?;
        return Ok(());
    };

    if !request_line.starts_with("POST ") {
        write_status(&mut write_half, 405, "method not allowed").await?;
        return Ok(());
    }

    // Route by path. OpenCode and other clients using `@ai-sdk/openai`
    // hit the new `/v1/responses` endpoint; clients on `@ai-sdk/openai-
    // compatible` or older OpenAI libs hit `/v1/chat/completions`.
    // The Script DSL is shared — only the wire encoding differs.
    let path = request_line.split_whitespace().nth(1).unwrap_or("");
    let is_responses_api = path.contains("/responses");

    match (is_responses_api, stream_requested) {
        (true, _) => write_openai_responses_sse(&mut write_half, &reply, &body_json).await?,
        (false, true) => write_openai_sse(&mut write_half, &reply).await?,
        (false, false) => write_openai_single(&mut write_half, &reply).await?,
    }
    Ok(())
}

async fn write_status(w: &mut tokio::net::tcp::OwnedWriteHalf, code: u16, msg: &str) -> Result<()> {
    let body = msg.as_bytes();
    let resp = format!(
        "HTTP/1.1 {code} {msg}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len(),
    );
    w.write_all(resp.as_bytes()).await?;
    w.write_all(body).await?;
    w.flush().await?;
    Ok(())
}

/// Streaming SSE response shaped like the `OpenAI` Chat Completions API.
async fn write_openai_sse(w: &mut tokio::net::tcp::OwnedWriteHalf, reply: &Reply) -> Result<()> {
    // Status + headers. `Transfer-Encoding: chunked` for HTTP/1.1; the
    // SSE body itself is plain.
    let headers = "HTTP/1.1 200 OK\r\n\
        Content-Type: text/event-stream\r\n\
        Cache-Control: no-cache\r\n\
        Transfer-Encoding: chunked\r\n\
        Connection: close\r\n\r\n";
    w.write_all(headers.as_bytes()).await?;

    let id = format!("chatcmpl-fake-{}", short_id());
    let created = chrono::Utc::now().timestamp();
    let model = "fake-inference";

    // First chunk often carries the role; OpenAI clients tolerate
    // either pattern. We send role on the opening delta.
    let mut sent_role = false;
    for chunk in &reply.chunks {
        let delta = if sent_role {
            serde_json::json!({ "content": chunk })
        } else {
            sent_role = true;
            serde_json::json!({ "role": "assistant", "content": chunk })
        };
        let event = OpenAiChunk {
            id: id.clone(),
            object: "chat.completion.chunk".into(),
            created,
            model: model.into(),
            choices: vec![Choice {
                index: 0,
                delta,
                finish_reason: None,
            }],
        };
        write_chunked_sse(w, &serde_json::to_string(&event)?).await?;
        if reply.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(reply.delay_ms)).await;
        }
    }

    // Final chunk with stop reason.
    let final_event = OpenAiChunk {
        id,
        object: "chat.completion.chunk".into(),
        created,
        model: model.into(),
        choices: vec![Choice {
            index: 0,
            delta: serde_json::json!({}),
            finish_reason: Some(reply.finish_reason.clone()),
        }],
    };
    write_chunked_sse(w, &serde_json::to_string(&final_event)?).await?;
    write_chunked_sse(w, "[DONE]").await?;

    // Final 0-length chunk to close the chunked transfer.
    w.write_all(b"0\r\n\r\n").await?;
    w.flush().await?;
    Ok(())
}

/// Non-streaming JSON response.
async fn write_openai_single(w: &mut tokio::net::tcp::OwnedWriteHalf, reply: &Reply) -> Result<()> {
    let text: String = reply.chunks.join("");
    let body = serde_json::json!({
        "id": format!("chatcmpl-fake-{}", short_id()),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": "fake-inference",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": reply.finish_reason.clone(),
        }],
        "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 },
    });
    let body_str = body.to_string();
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body_str.len(),
    );
    w.write_all(headers.as_bytes()).await?;
    w.write_all(body_str.as_bytes()).await?;
    w.flush().await?;
    Ok(())
}

/// Write one SSE event as an HTTP/1.1 chunked-transfer chunk.
async fn write_chunked_sse(w: &mut tokio::net::tcp::OwnedWriteHalf, data: &str) -> io::Result<()> {
    // SSE wire shape: `data: <payload>\n\n`.
    let payload = format!("data: {data}\n\n");
    let size_hex = format!("{:x}\r\n", payload.len());
    w.write_all(size_hex.as_bytes()).await?;
    w.write_all(payload.as_bytes()).await?;
    w.write_all(b"\r\n").await?;
    Ok(())
}

/// Write one SSE event with `event: NAME\n` prefix as a chunked-
/// transfer chunk. The Responses API uses the typed-event SSE form
/// (where the SSE `event:` line carries the type and the `data:`
/// payload is the typed JSON object).
async fn write_chunked_typed_event(
    w: &mut tokio::net::tcp::OwnedWriteHalf,
    event_name: &str,
    data: &str,
) -> io::Result<()> {
    let payload = format!("event: {event_name}\ndata: {data}\n\n");
    let size_hex = format!("{:x}\r\n", payload.len());
    w.write_all(size_hex.as_bytes()).await?;
    w.write_all(payload.as_bytes()).await?;
    w.write_all(b"\r\n").await?;
    Ok(())
}

/// Streaming SSE response shaped like the `OpenAI` Responses API
/// (POST /v1/responses, the post-2025 replacement for chat-completions).
///
/// Event order matters: clients (including the `@ai-sdk/openai`
/// provider used by `OpenCode`) validate the sequence via Zod schemas
/// and bail silently if any required field is missing.
///
/// Stream shape we emit for a single text reply:
///
/// ```text
///   event: response.created
///   event: response.in_progress
///   event: response.output_item.added         (item: message, role=assistant)
///   event: response.content_part.added        (part: output_text)
///   event: response.output_text.delta         (delta: <chunk text>)  × N
///   event: response.output_text.done          (final text)
///   event: response.content_part.done
///   event: response.output_item.done
///   event: response.completed                 (usage stats)
/// ```
///
/// For a tool-call reply (`reply.tool_call` set) the inner shape is
/// instead:
///
/// ```text
///   event: response.output_item.added         (item: function_call, args="")
///   event: response.function_call_arguments.delta   (delta: <args JSON>)
///   event: response.function_call_arguments.done    (arguments: <full>)
///   event: response.output_item.done          (item: function_call, completed)
/// ```
///
/// `request_body` lets us echo back the requested `model` so the
/// validator at the client is happy.
async fn write_openai_responses_sse(
    w: &mut tokio::net::tcp::OwnedWriteHalf,
    reply: &Reply,
    request_body: &Value,
) -> Result<()> {
    let headers = "HTTP/1.1 200 OK\r\n\
        Content-Type: text/event-stream\r\n\
        Cache-Control: no-cache\r\n\
        Transfer-Encoding: chunked\r\n\
        Connection: close\r\n\r\n";
    w.write_all(headers.as_bytes()).await?;

    let model = request_body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("fake-model")
        .to_string();
    let response_id = format!("resp_{}", short_id());
    // For text replies this is the message id; for tool-call replies
    // it's the function_call item id (prefix `fc_`).
    let is_tool_call = reply.tool_call.is_some();
    let item_id = if is_tool_call {
        format!("fc_{}", short_id())
    } else {
        format!("msg_{}", short_id())
    };
    let created_at = chrono::Utc::now().timestamp();

    // The accumulating text seen so far — `response.completed` carries
    // the full message object with all content, so we build it up as
    // deltas go out.
    let mut accumulated = String::new();
    let mut seq: u64 = 0;

    // Helper closures would be cleaner but borrow-check noise; expand inline.
    let make_response_obj = |status: &str, full_text: &str| -> Value {
        let output = if status != "completed" {
            serde_json::json!([])
        } else if let Some(tc) = &reply.tool_call {
            serde_json::json!([{
                "id": &item_id,
                "type": "function_call",
                "status": "completed",
                "call_id": &tc.call_id,
                "name": &tc.name,
                "arguments": &tc.arguments,
            }])
        } else {
            serde_json::json!([{
                "id": &item_id,
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": [{"type": "output_text", "text": full_text, "annotations": []}],
            }])
        };
        serde_json::json!({
            "id": &response_id,
            "object": "response",
            "created_at": created_at,
            "status": status,
            "model": &model,
            "output": output,
            "parallel_tool_calls": true,
            "tool_choice": "auto",
            "tools": [],
            "usage": if status == "completed" {
                serde_json::json!({
                    "input_tokens": 1, "output_tokens": 1, "total_tokens": 2
                })
            } else { serde_json::Value::Null },
        })
    };

    // response.created
    let payload = serde_json::json!({
        "type": "response.created",
        "sequence_number": seq,
        "response": make_response_obj("in_progress", ""),
    });
    write_chunked_typed_event(w, "response.created", &payload.to_string()).await?;
    seq += 1;

    // response.in_progress
    let payload = serde_json::json!({
        "type": "response.in_progress",
        "sequence_number": seq,
        "response": make_response_obj("in_progress", ""),
    });
    write_chunked_typed_event(w, "response.in_progress", &payload.to_string()).await?;
    seq += 1;

    if let Some(tc) = &reply.tool_call {
        // Tool-call event sequence. Differs from text in that the
        // output item is `type: function_call` and the delta events
        // stream argument JSON instead of text.

        // response.output_item.added — function_call item, empty args
        let payload = serde_json::json!({
            "type": "response.output_item.added",
            "sequence_number": seq,
            "output_index": 0,
            "item": {
                "id": &item_id,
                "type": "function_call",
                "status": "in_progress",
                "call_id": &tc.call_id,
                "name": &tc.name,
                "arguments": "",
            },
        });
        write_chunked_typed_event(w, "response.output_item.added", &payload.to_string()).await?;
        seq += 1;

        // response.function_call_arguments.delta — single big chunk.
        // Real models stream char-by-char but clients accept either.
        let payload = serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "sequence_number": seq,
            "item_id": &item_id,
            "output_index": 0,
            "delta": &tc.arguments,
        });
        write_chunked_typed_event(
            w,
            "response.function_call_arguments.delta",
            &payload.to_string(),
        )
        .await?;
        seq += 1;

        // response.function_call_arguments.done — final args string.
        let payload = serde_json::json!({
            "type": "response.function_call_arguments.done",
            "sequence_number": seq,
            "item_id": &item_id,
            "output_index": 0,
            "arguments": &tc.arguments,
        });
        write_chunked_typed_event(
            w,
            "response.function_call_arguments.done",
            &payload.to_string(),
        )
        .await?;
        seq += 1;

        // response.output_item.done — function_call item, completed
        let payload = serde_json::json!({
            "type": "response.output_item.done",
            "sequence_number": seq,
            "output_index": 0,
            "item": {
                "id": &item_id,
                "type": "function_call",
                "status": "completed",
                "call_id": &tc.call_id,
                "name": &tc.name,
                "arguments": &tc.arguments,
            },
        });
        write_chunked_typed_event(w, "response.output_item.done", &payload.to_string()).await?;
        seq += 1;
    } else {
        // Text reply event sequence.

        // response.output_item.added (the message item)
        let payload = serde_json::json!({
            "type": "response.output_item.added",
            "sequence_number": seq,
            "output_index": 0,
            "item": {
                "id": &item_id,
                "type": "message",
                "status": "in_progress",
                "role": "assistant",
                "content": [],
            },
        });
        write_chunked_typed_event(w, "response.output_item.added", &payload.to_string()).await?;
        seq += 1;

        // response.content_part.added (output_text part)
        let payload = serde_json::json!({
            "type": "response.content_part.added",
            "sequence_number": seq,
            "item_id": &item_id,
            "output_index": 0,
            "content_index": 0,
            "part": {"type": "output_text", "text": "", "annotations": []},
        });
        write_chunked_typed_event(w, "response.content_part.added", &payload.to_string()).await?;
        seq += 1;

        // response.output_text.delta × N
        for chunk in &reply.chunks {
            accumulated.push_str(chunk);
            let payload = serde_json::json!({
                "type": "response.output_text.delta",
                "sequence_number": seq,
                "item_id": &item_id,
                "output_index": 0,
                "content_index": 0,
                "delta": chunk,
            });
            write_chunked_typed_event(w, "response.output_text.delta", &payload.to_string())
                .await?;
            seq += 1;
            if reply.delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(reply.delay_ms)).await;
            }
        }

        // response.output_text.done
        let payload = serde_json::json!({
            "type": "response.output_text.done",
            "sequence_number": seq,
            "item_id": &item_id,
            "output_index": 0,
            "content_index": 0,
            "text": &accumulated,
        });
        write_chunked_typed_event(w, "response.output_text.done", &payload.to_string()).await?;
        seq += 1;

        // response.content_part.done
        let payload = serde_json::json!({
            "type": "response.content_part.done",
            "sequence_number": seq,
            "item_id": &item_id,
            "output_index": 0,
            "content_index": 0,
            "part": {"type": "output_text", "text": &accumulated, "annotations": []},
        });
        write_chunked_typed_event(w, "response.content_part.done", &payload.to_string()).await?;
        seq += 1;

        // response.output_item.done
        let payload = serde_json::json!({
            "type": "response.output_item.done",
            "sequence_number": seq,
            "output_index": 0,
            "item": {
                "id": &item_id,
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": [{"type": "output_text", "text": &accumulated, "annotations": []}],
            },
        });
        write_chunked_typed_event(w, "response.output_item.done", &payload.to_string()).await?;
        seq += 1;
    }

    // response.completed
    let payload = serde_json::json!({
        "type": "response.completed",
        "sequence_number": seq,
        "response": make_response_obj("completed", &accumulated),
    });
    write_chunked_typed_event(w, "response.completed", &payload.to_string()).await?;

    // Close chunked transfer.
    w.write_all(b"0\r\n\r\n").await?;
    w.flush().await?;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAiChunk {
    id: String,
    object: String,
    created: i64,
    model: String,
    choices: Vec<Choice>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Choice {
    index: u32,
    delta: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    finish_reason: Option<String>,
}

fn short_id() -> String {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spin up a server, hit it with a manually-crafted SSE request,
    /// confirm we get a `data:` line back with our scripted content.
    #[tokio::test]
    async fn fake_server_streams_canned_reply() -> Result<()> {
        let script = Script::new().stream(["hello", " world"]);
        let server = FakeServer::start(script).await?;

        let url = server.url();
        let host_port = url.trim_start_matches("http://");
        let mut conn = TcpStream::connect(host_port).await?;
        // Compose request carefully so Content-Length matches the body
        // byte count — read_exact on the server will otherwise hang.
        let body = br#"{"stream":true,"model":"m"}"#;
        let request = format!(
            "POST /v1/chat/completions HTTP/1.1\r\n\
             Host: localhost\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n",
            body.len(),
        );
        conn.write_all(request.as_bytes()).await?;
        conn.write_all(body).await?;

        let mut response = Vec::new();
        conn.read_to_end(&mut response).await?;
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "got: {text}");
        assert!(text.contains("text/event-stream"), "got: {text}");
        assert!(text.contains("hello"), "missing first chunk; got: {text}");
        assert!(text.contains("world"), "missing second chunk; got: {text}");
        assert!(text.contains("[DONE]"), "missing terminator; got: {text}");
        Ok(())
    }

    #[tokio::test]
    async fn fake_server_non_streaming_returns_complete_json() -> Result<()> {
        let script = Script::new().say("complete answer");
        let server = FakeServer::start(script).await?;
        let host_port = server.url().trim_start_matches("http://").to_string();

        let mut conn = TcpStream::connect(&host_port).await?;
        let body = br#"{"model":"m"}"#;
        let request = format!(
            "POST /v1/chat/completions HTTP/1.1\r\n\
             Host: localhost\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n",
            body.len(),
        );
        conn.write_all(request.as_bytes()).await?;
        conn.write_all(body).await?;

        let mut response = Vec::new();
        conn.read_to_end(&mut response).await?;
        let text = String::from_utf8_lossy(&response);
        assert!(text.contains("application/json"), "got: {text}");
        assert!(text.contains("complete answer"), "got: {text}");
        assert!(text.contains("\"finish_reason\":\"stop\""), "got: {text}");
        Ok(())
    }

    #[tokio::test]
    async fn fake_server_returns_500_on_empty_script() -> Result<()> {
        let server = FakeServer::start(Script::new()).await?;
        let host_port = server.url().trim_start_matches("http://").to_string();

        let mut conn = TcpStream::connect(&host_port).await?;
        let body = b"{}";
        let request = format!(
            "POST /v1/chat/completions HTTP/1.1\r\n\
             Host: localhost\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n",
            body.len(),
        );
        conn.write_all(request.as_bytes()).await?;
        conn.write_all(body).await?;
        let mut response = Vec::new();
        conn.read_to_end(&mut response).await?;
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 500"), "got: {text}");
        Ok(())
    }
}
