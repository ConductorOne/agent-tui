//! `send_ansi` + `resize` handlers.
//!
//! `send_ansi` is the escape hatch for sequences our key-token grammar
//! doesn't cover (mouse events, OSC requests, DCS payloads). `resize` keeps
//! the engine grid and the PTY winsize in sync — kernel sends SIGWINCH to
//! the child as a side effect.

use std::sync::Arc;

use agent_tui_protocol::{ErrorBody, ErrorCode, PaneId, Response};

use crate::pane::{Pane, Registry};

/// `send_ansi` — decode `bytes_hex` and write to the PTY master end.
pub async fn send_ansi(
    registry: &Arc<Registry>,
    pane: Option<PaneId>,
    bytes_hex: String,
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
    let pane_arc = match resolve(registry, pane).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
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
    let pane_arc = match resolve(registry, pane).await {
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

async fn resolve(registry: &Arc<Registry>, pane: Option<PaneId>) -> Result<Arc<Pane>, Response> {
    if let Some(id) = pane {
        return registry.get(&id).await.ok_or_else(|| {
            Response::err(ErrorBody::new(
                ErrorCode::NoActivePane,
                format!("pane {id} not found"),
                "call list to see live panes",
            ))
        });
    }
    let list = registry.list().await;
    match list.len() {
        1 => registry.get(&list[0].id).await.ok_or_else(|| {
            Response::err(ErrorBody::new(
                ErrorCode::NoActivePane,
                "pane disappeared",
                "retry",
            ))
        }),
        0 => Err(Response::err(ErrorBody::new(
            ErrorCode::NoActivePane,
            "no panes",
            "spawn a pane first",
        ))),
        _ => Err(Response::err(ErrorBody::new(
            ErrorCode::NoActivePane,
            "multiple panes; --pane required",
            "pass --pane p<N>",
        ))),
    }
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
