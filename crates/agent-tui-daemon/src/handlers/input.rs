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

use agent_tui_adapter::RoutedStep;
use agent_tui_protocol::{
    ErrorBody, ErrorCode, PaneId, Response, Selector, Warning, format_selector_parse_error, keymap,
    outline_all_refs,
};

use crate::governance::{Governance, build};
use crate::pane::{Pane, Registry, resolve_focused};

const BARRIER_TIMEOUT_MS: u64 = 200;

/// `press` — parse a key-token sequence and feed it to the pane.
pub async fn press(
    registry: &Arc<Registry>,
    governance: &Governance,
    pane: Option<PaneId>,
    keys: String,
    to: Option<String>,
    lease: Option<uuid::Uuid>,
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
    deliver(registry, governance, pane, &bytes, key_tokens, to, lease).await
}

/// `type` — write literal UTF-8 text to the pane (no key interpretation).
pub async fn type_text(
    registry: &Arc<Registry>,
    governance: &Governance,
    pane: Option<PaneId>,
    text: String,
    to: Option<String>,
    lease: Option<uuid::Uuid>,
) -> Response {
    deliver(registry, governance, pane, text.as_bytes(), None, to, lease).await
}

async fn deliver(
    registry: &Arc<Registry>,
    governance: &Governance,
    pane: Option<PaneId>,
    bytes: &[u8],
    key_tokens: Option<Vec<String>>,
    to: Option<String>,
    lease: Option<uuid::Uuid>,
) -> Response {
    let pane_arc = match resolve_focused(registry, pane).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    if let Some(resp) = crate::handlers::lease_denied(&pane_arc, lease) {
        return resp;
    }

    let decision = governance
        .check(build::input(pane_arc.id.clone(), bytes, key_tokens))
        .await;
    if let Some(resp) = policy_response(&decision) {
        return resp;
    }

    // No `--to`: direct PTY write, classic barrier semantics.
    let Some(target_sel) = to else {
        return write_with_barrier(&pane_arc, bytes).await;
    };

    // Routed delivery: resolve selector → ref, ask adapter to translate
    // into a step list, execute it.
    let target_ref = match resolve_target_ref(&pane_arc, &target_sel).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let adapter = pane_arc.adapter().await;
    let snap = pane_arc.engine.snapshot();
    let steps = match adapter.route(&snap, &target_ref, bytes).await {
        Ok(s) => s,
        Err(e) => {
            return Response::err(ErrorBody::new(
                ErrorCode::AdapterFailed,
                format!("adapter route failed: {e}"),
                "adapter does not support this routing",
            ));
        }
    };

    execute_routed_steps(&pane_arc, &steps, bytes.len()).await
}

/// Resolve a `--to <selector>` to a concrete ref-path string. The first
/// match in depth-first pre-order wins.
async fn resolve_target_ref(pane: &Pane, selector: &str) -> Result<String, Response> {
    let sel = Selector::parse(selector).map_err(|e| {
        let formatted = format_selector_parse_error(selector, &e);
        Response::err(ErrorBody::new(
            ErrorCode::InvalidArgs,
            formatted,
            "see docs/design/addressing-rfc.md §2.2 or `agent-tui skills get addressing`",
        ))
    })?;
    let adapter = pane.adapter().await;
    let snap = pane.engine.snapshot();
    let outline = adapter.outline(&snap).await.map_err(|e| {
        Response::err(ErrorBody::new(
            ErrorCode::AdapterFailed,
            format!("adapter outline failed: {e}"),
            "adapter could not provide an outline for routing",
        ))
    })?;
    if let Some(node) = sel.first(&outline) {
        return Ok(node.r#ref.clone());
    }
    let refs = outline_all_refs(&outline, 20);
    let hint = if refs.is_empty() {
        "snapshot to inspect the outline; relax the selector".to_string()
    } else {
        format!("available refs: {}", refs.join(", "))
    };
    Err(Response::err(ErrorBody::new(
        ErrorCode::RoutingUnsupported,
        format!("--to selector {selector:?} matched no node"),
        hint,
    )))
}

async fn execute_routed_steps(pane: &Pane, steps: &[RoutedStep], hint_len: usize) -> Response {
    let mut sub = pane.engine.subscribe();
    let pre_seq = pane.engine.snapshot().sequence;
    let mut total_written = 0usize;

    for step in steps {
        match step {
            RoutedStep::Write { bytes } => {
                if let Err(e) = pane.pty.write_input(bytes) {
                    return Response::err(ErrorBody::new(
                        ErrorCode::Internal,
                        format!("pty write failed mid-route: {e}"),
                        "child may have exited; call list",
                    ));
                }
                total_written += bytes.len();
            }
            RoutedStep::Delay { ms } => {
                tokio::time::sleep(Duration::from_millis(u64::from(*ms))).await;
            }
            RoutedStep::WaitFor {
                selector,
                max_wait_ms,
            } => {
                if let Err(resp) = wait_for_selector(pane, selector, *max_wait_ms).await {
                    return resp;
                }
            }
        }
    }

    // Single barrier at the end of the route, mirroring the unrouted
    // path's "PTY changed something" guarantee.
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
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }

    let post_seq = pane.engine.snapshot().sequence;
    let mut resp = Response::ok(serde_json::json!({
        "pane": pane.id,
        "bytes_written": total_written,
        "bytes_requested": hint_len,
        "pre_sequence": pre_seq,
        "post_sequence": post_seq,
        "barrier_observed": observed.is_some(),
        "routed": true,
    }));
    if observed.is_none() && post_seq == pre_seq {
        resp = resp.with_warning(Warning {
            code: "no_echo_within_barrier".into(),
            message: format!("no engine mutation within {BARRIER_TIMEOUT_MS}ms after routed write"),
        });
    }
    resp
}

async fn wait_for_selector(pane: &Pane, selector: &str, max_wait_ms: u32) -> Result<(), Response> {
    let sel = Selector::parse(selector).map_err(|e| {
        Response::err(ErrorBody::new(
            ErrorCode::AdapterFailed,
            format!(
                "adapter emitted a WaitFor with an unparseable selector at byte {}: {}",
                e.at, e.kind
            ),
            "adapter bug; report it",
        ))
    })?;
    let adapter = pane.adapter().await;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(u64::from(max_wait_ms));
    let mut sub = pane.engine.subscribe();
    loop {
        let snap = pane.engine.snapshot();
        if let Ok(outline) = adapter.outline(&snap).await {
            if sel.first(&outline).is_some() {
                return Ok(());
            }
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(Response::err(ErrorBody::new(
                ErrorCode::RoutingGateTimeout,
                format!("WaitFor selector {selector:?} did not match within {max_wait_ms}ms"),
                "adapter's routing gate timed out",
            )));
        }
        if tokio::time::timeout(remaining, sub.recv()).await.is_err() {
            // hit the timeout — re-check once more then bail next iter
        }
    }
}

/// Direct-write path used when `--to` is unset. Same press-then-quiesce
/// barrier as before.
async fn write_with_barrier(pane: &Pane, bytes: &[u8]) -> Response {
    let mut sub = pane.engine.subscribe();
    let pre_seq = pane.engine.snapshot().sequence;
    if let Err(e) = pane.pty.write_input(bytes) {
        return Response::err(ErrorBody::new(
            ErrorCode::Internal,
            format!("pty write failed: {e}"),
            "child may have exited; call list",
        ));
    }
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
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }
    let post_seq = pane.engine.snapshot().sequence;
    let mut resp = Response::ok(serde_json::json!({
        "pane": pane.id,
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
