//! `press` / `type` command handlers.
//!
//! Both share the press-then-quiesce barrier (RFC §4.3):
//!  1. Subscribe to the engine's mutation stream BEFORE writing.
//!  2. Write bytes to the PTY master.
//!  3. Block on the next `MutationEvent` whose sequence exceeds the
//!     pre-write sequence — bounded by `--timeout` (default 200ms).
//!
//! If the child stays silent past the timeout, the handler still returns
//! success but attaches a `warning` so callers can branch.

use std::sync::Arc;
use std::time::Duration;

use agent_tui_protocol::{ErrorBody, ErrorCode, PaneId, Response, Warning, keymap};

use crate::governance::{Governance, build};
use crate::pane::{Registry, resolve_focused};

const BARRIER_TIMEOUT_MS: u64 = 200;

/// `press` — parse a key-token sequence and feed it to the pane.
pub async fn press(
    registry: &Arc<Registry>,
    governance: &Governance,
    pane: Option<PaneId>,
    keys: String,
) -> Response {
    let tokens = match keymap::parse(&keys) {
        Ok(t) => t,
        Err(e) => {
            return Response::err(ErrorBody::new(
                ErrorCode::KeyFormatError,
                e.to_string(),
                "see skill-data/core/references/keymap.md for valid tokens",
            ));
        }
    };
    let bytes = keymap::serialize(&tokens);
    let key_tokens = Some(tokens.iter().map(|t| format!("{t:?}")).collect());
    deliver(registry, governance, pane, &bytes, key_tokens).await
}

/// `type` — write literal UTF-8 text to the pane (no key interpretation).
pub async fn type_text(
    registry: &Arc<Registry>,
    governance: &Governance,
    pane: Option<PaneId>,
    text: String,
) -> Response {
    deliver(registry, governance, pane, text.as_bytes(), None).await
}

async fn deliver(
    registry: &Arc<Registry>,
    governance: &Governance,
    pane: Option<PaneId>,
    bytes: &[u8],
    key_tokens: Option<Vec<String>>,
) -> Response {
    let pane_arc = match resolve_focused(registry, pane).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let decision = governance
        .check(build::input(pane_arc.id.clone(), bytes, key_tokens))
        .await;
    if let Some(resp) = policy_response(&decision) {
        return resp;
    }

    // Step 1: subscribe BEFORE writing so we can't miss the mutation.
    let mut sub = pane_arc.engine.subscribe();
    let pre_seq = pane_arc.engine.snapshot().sequence;

    // Step 2: write.
    if let Err(e) = pane_arc.pty.write_input(bytes) {
        return Response::err(ErrorBody::new(
            ErrorCode::Internal,
            format!("pty write failed: {e}"),
            "child may have exited; call list",
        ));
    }

    // Step 3: wait for next mutation past `pre_seq`, bounded by timeout.
    let timeout = Duration::from_millis(BARRIER_TIMEOUT_MS);
    let mut observed = None;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, sub.recv()).await {
            Ok(Ok(evt)) if evt.sequence > pre_seq => {
                observed = Some(evt);
                break;
            }
            Ok(Ok(_)) => {}               // stale event — wait for the next
            Ok(Err(_)) | Err(_) => break, // channel closed or timeout
        }
    }

    let post_seq = pane_arc.engine.snapshot().sequence;
    let mut resp = Response::ok(serde_json::json!({
        "pane": pane_arc.id,
        "bytes_written": bytes.len(),
        "pre_sequence": pre_seq,
        "post_sequence": post_seq,
        "barrier_observed": observed.is_some(),
    }));

    if observed.is_none() && post_seq == pre_seq {
        resp = resp.with_warning(Warning {
            code: "no_echo_within_barrier".to_string(),
            message: format!(
                "no engine mutation within {BARRIER_TIMEOUT_MS}ms; child may be blocked or silent"
            ),
        });
    }
    resp
}

fn policy_response(decision: &agent_tui_protocol::Decision) -> Option<Response> {
    use agent_tui_protocol::Verdict;
    match decision.verdict {
        Verdict::Allow => None,
        Verdict::Deny => Some(Response::err(ErrorBody::new(
            ErrorCode::PolicyDenied,
            decision.reason.clone(),
            "input blocked by policy",
        ))),
        Verdict::RequireConfirm => Some(Response::err(ErrorBody::new(
            ErrorCode::PolicyPending,
            decision.reason.clone(),
            "human confirmation required",
        ))),
    }
}
