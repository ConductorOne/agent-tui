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

/// One server-side reply slot: a sequence of SSE content chunks plus
/// optional structured fields (tool calls, stop reason).
#[derive(Debug, Clone, Default)]
pub struct Reply {
    /// Successive text chunks that get streamed as
    /// `delta.content` SSE events.
    pub chunks: Vec<String>,
    /// Optional per-chunk latency. Defaults to 5ms if unset.
    pub delay_ms: u64,
    /// `finish_reason` in the final SSE chunk. Default `"stop"`.
    pub finish_reason: String,
}

impl Reply {
    /// Whole reply as one chunk, no delay.
    pub fn one(text: impl Into<String>) -> Self {
        Self {
            chunks: vec![text.into()],
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
            delay_ms: 5,
            finish_reason: "stop".into(),
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
}

/// Running fake-inference server.
pub struct FakeServer {
    addr: SocketAddr,
    shutdown_tx: broadcast::Sender<()>,
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

        let shutdown = shutdown_tx.clone();
        let accept = tokio::spawn(async move {
            loop {
                let mut shutdown_rx = shutdown.subscribe();
                tokio::select! {
                    _ = shutdown_rx.recv() => break,
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((stream, _peer)) => {
                                let script = script.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = handle_connection(stream, script).await {
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

async fn handle_connection(stream: TcpStream, script: Arc<Mutex<ScriptState>>) -> Result<()> {
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

    if stream_requested {
        write_openai_sse(&mut write_half, &reply).await?;
    } else {
        write_openai_single(&mut write_half, &reply).await?;
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
