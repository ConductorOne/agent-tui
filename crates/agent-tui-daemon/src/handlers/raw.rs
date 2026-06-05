//! `send_ansi` + `resize` handlers.
//!
//! `send_ansi` is the escape hatch for sequences our key-token grammar
//! doesn't cover (mouse events, OSC requests, DCS payloads). `resize` keeps
//! the engine grid and the PTY winsize in sync — kernel sends SIGWINCH to
//! the child as a side effect.

use std::sync::Arc;

use agent_tui_protocol::{ErrorBody, ErrorCode, PaneId, Response};

use crate::pane::{Registry, resolve_focused};

/// `send_ansi` — decode `bytes_hex` and write to the PTY master end.
pub async fn send_ansi(
    registry: &Arc<Registry>,
    pane: Option<PaneId>,
    bytes_hex: String,
    lease: Option<uuid::Uuid>,
) -> Response {
    let bytes = match hex_decode(&bytes_hex) {
        Ok(b) => b,
        Err(reason) => {
            return Response::err(ErrorBody::new(
                ErrorCode::InvalidArgs,
                reason,
                "bytes_hex must be a hex string with optional whitespace",
            ));
        }
    };
    let pane_arc = match resolve_focused(registry, pane).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if let Some(resp) = super::lease_denied(&pane_arc, lease) {
        return resp;
    }
    if let Err(e) = pane_arc.pty.write_input(&bytes) {
        return Response::err(ErrorBody::new(
            ErrorCode::Internal,
            format!("pty write failed: {e}"),
            "child may have exited; call list",
        ));
    }
    Response::ok(serde_json::json!({
        "pane": pane_arc.id,
        "bytes_written": bytes.len(),
    }))
}

/// `resize` — propagate the new geometry to both engine and PTY kernel side.
pub async fn resize(
    registry: &Arc<Registry>,
    pane: Option<PaneId>,
    cols: u16,
    rows: u16,
) -> Response {
    if cols < 2 || rows < 1 {
        return Response::err(ErrorBody::new(
            ErrorCode::InvalidArgs,
            "resize requires cols >= 2 and rows >= 1",
            "agent terminals expect minimum geometry",
        ));
    }
    let pane_arc = match resolve_focused(registry, pane).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if let Err(e) = pane_arc.pty.resize(cols, rows) {
        return Response::err(ErrorBody::new(
            ErrorCode::Internal,
            format!("pty resize failed: {e}"),
            "pane may be invalid",
        ));
    }
    if let Err(e) = pane_arc.engine.resize(cols, rows) {
        return Response::err(ErrorBody::new(
            ErrorCode::Internal,
            format!("engine resize failed: {e}"),
            "engine refused the new geometry",
        ));
    }
    Response::ok(serde_json::json!({
        "pane": pane_arc.id,
        "cols": cols,
        "rows": rows,
    }))
}

/// `stdin` — write bytes to the child's stdin pipe (Pipe-mode panes).
pub async fn stdin(
    registry: &Arc<Registry>,
    pane: Option<PaneId>,
    bytes_hex: String,
    lease: Option<uuid::Uuid>,
) -> Response {
    let bytes = match hex_decode(&bytes_hex) {
        Ok(b) => b,
        Err(reason) => {
            return Response::err(ErrorBody::new(
                ErrorCode::InvalidArgs,
                reason,
                "bytes_hex must be a hex string with optional whitespace",
            ));
        }
    };
    let pane_arc = match resolve_focused(registry, pane).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if let Some(resp) = super::lease_denied(&pane_arc, lease) {
        return resp;
    }
    if let Err(e) = pane_arc.pty.write_stdin_pipe(&bytes) {
        return Response::err(ErrorBody::new(
            ErrorCode::InvalidArgs,
            format!("{e}"),
            "respawn the pane with `--stdin pipe` to use this verb",
        ));
    }
    Response::ok(serde_json::json!({
        "pane": pane_arc.id,
        "bytes_written": bytes.len(),
    }))
}

/// `tail` — return raw output bytes since `since`. Pairs with the
/// per-pane output ring buffer in pty.rs. Use `strip_ansi` to get
/// plain text instead of bytes-with-escape-sequences.
pub async fn tail(
    registry: &Arc<Registry>,
    pane: Option<PaneId>,
    since: u64,
    strip_ansi: bool,
) -> Response {
    let pane_arc = match resolve_focused(registry, pane).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let read = pane_arc.pty.tail(since);
    let payload: serde_json::Value = if strip_ansi {
        let text = strip_ansi_bytes(&read.bytes);
        serde_json::json!({
            "pane": pane_arc.id,
            "text": text,
            "next_since": read.total,
            "lost_bytes": read.lost_bytes,
        })
    } else {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&read.bytes);
        serde_json::json!({
            "pane": pane_arc.id,
            "bytes_b64": encoded,
            "next_since": read.total,
            "lost_bytes": read.lost_bytes,
        })
    };
    Response::ok(payload)
}

/// Public re-export of [`strip_ansi_bytes`] for the streaming `tail
/// --follow` path in `server.rs`. Kept under a distinct name so
/// renames here propagate visibly to that caller.
#[must_use]
pub(crate) fn strip_ansi_for_streaming(bytes: &[u8]) -> String {
    strip_ansi_bytes(bytes)
}

/// Strip CSI/SGR/OSC escape sequences and return the remaining UTF-8
/// text. `\r\n` → `\n` so the output is POSIX-flavored. Non-UTF-8
/// bytes are replaced via `from_utf8_lossy`.
fn strip_ansi_bytes(bytes: &[u8]) -> String {
    let mut cleaned = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1B {
            // ESC <next> — skip the sequence.
            i += 1;
            if i >= bytes.len() {
                break;
            }
            let next = bytes[i];
            i += 1;
            match next {
                // CSI: 0x1B 0x5B ... <final 0x40..0x7E>
                b'[' => {
                    while i < bytes.len() {
                        let c = bytes[i];
                        i += 1;
                        if (0x40..=0x7E).contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: 0x1B 0x5D ... BEL or ESC \
                b']' => {
                    while i < bytes.len() {
                        let c = bytes[i];
                        if c == 0x07 {
                            i += 1;
                            break;
                        }
                        if c == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                // 3-byte char-set / line-attribute escapes:
                //   ESC ( <selector>  (G0 designation)
                //   ESC ) <selector>  (G1)
                //   ESC * <selector>  (G2)
                //   ESC + <selector>  (G3)
                //   ESC # <param>     (DEC line attrs)
                // Each consumes one more byte (the selector).
                b'(' | b')' | b'*' | b'+' | b'#' if i < bytes.len() => {
                    i += 1;
                }
                // 2-byte escapes: nothing more to consume.
                _ => {}
            }
        } else if b == b'\r' {
            cleaned.push(b'\n');
            i += 1;
            if i < bytes.len() && bytes[i] == b'\n' {
                i += 1;
            }
        } else if b == b'\n' || b == b'\t' || b >= 0x20 {
            cleaned.push(b);
            i += 1;
        } else {
            // Drop other C0 controls (backspace, vertical-tab, …).
            i += 1;
        }
    }
    String::from_utf8_lossy(&cleaned).into_owned()
}

/// `close-stdin` — close the child's stdin pipe (EOF). No-op for
/// non-pipe panes.
pub async fn close_stdin(registry: &Arc<Registry>, pane: Option<PaneId>) -> Response {
    let pane_arc = match resolve_focused(registry, pane).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if let Err(e) = pane_arc.pty.close_stdin_pipe() {
        return Response::err(ErrorBody::new(
            ErrorCode::Internal,
            format!("close_stdin failed: {e}"),
            "pane may be invalid",
        ));
    }
    Response::ok(serde_json::json!({
        "pane": pane_arc.id,
        "stdin_mode": format!("{:?}", pane_arc.pty.stdin_mode()),
    }))
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    // Strip ASCII whitespace so agents can write "1b 5b 41" or "1b5b41".
    let cleaned: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if cleaned.len() % 2 != 0 {
        return Err("hex string has odd length".into());
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&cleaned[i..i + 2], 16)
                .map_err(|_| format!("invalid hex pair at offset {i}: '{}'", &cleaned[i..i + 2]))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_decode_with_and_without_spaces() {
        let expected: Vec<u8> = vec![0x1B, 0x5B, 0x41];
        assert_eq!(hex_decode("1b5b41").unwrap(), expected);
        assert_eq!(hex_decode("1b 5b 41").unwrap(), expected);
        assert_eq!(hex_decode("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn hex_decode_rejects_odd_length() {
        assert!(hex_decode("1b5").is_err());
    }

    #[test]
    fn hex_decode_rejects_garbage() {
        assert!(hex_decode("zz").is_err());
    }
}
