//! `wait` command handler.
//!
//! Subscribes to the pane's engine mutation stream and blocks until the
//! requested [`WaitCondition`] is satisfied, the per-call timeout fires
//! (returning `WAIT_TIMEOUT`), or the pane's child exits (returning
//! `PANE_DEAD` unless the condition is `Exit`).
//!
//! Conditions don't share the per-pane writer queue — snapshots are
//! lock-free against the engine and `subscribe()` returns a broadcast
//! receiver. Multiple waits on the same pane can coexist trivially.

use std::sync::Arc;
use std::time::Duration;

use agent_tui_engine::MutationEvent;
use agent_tui_protocol::request::WaitCondition;
use agent_tui_protocol::{ErrorBody, ErrorCode, PaneId, Response};
use regex::Regex;
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::Instant;

use crate::hash_window::HashWindow;
use crate::pane::{Pane, Registry};

/// Drive a [`WaitCondition`] to completion.
pub async fn run(
    registry: &Arc<Registry>,
    hashes: &Arc<HashWindow>,
    pane: Option<PaneId>,
    condition: WaitCondition,
    timeout: Duration,
) -> Response {
    let pane_arc = match resolve(registry, pane).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let condition = match resolve_hash(hashes, &pane_arc.id, condition).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };

    let deadline = Instant::now() + timeout;
    let sub = pane_arc.engine.subscribe();
    let outcome = drive(pane_arc.as_ref(), sub, condition, deadline).await;
    finish(pane_arc.as_ref(), &outcome)
}

/// Replace a `Hash` condition with the equivalent `Since` once the window
/// resolves the hash.
async fn resolve_hash(
    hashes: &Arc<HashWindow>,
    pane: &PaneId,
    cond: WaitCondition,
) -> Result<WaitCondition, Response> {
    if let WaitCondition::Hash { hash } = &cond {
        match hashes.lookup(pane, hash).await {
            Some(seq) => return Ok(WaitCondition::Since { since: seq }),
            None => {
                return Err(Response::err(ErrorBody::new(
                    ErrorCode::WaitHashUnknown,
                    format!("hash {hash} not in seq->hash window"),
                    "call snapshot to refresh the window",
                )));
            }
        }
    }
    Ok(cond)
}

/// Outcome of the wait loop.
#[derive(Debug)]
enum Outcome {
    Matched { sequence: u64 },
    Timeout,
    PaneDead,
}

async fn drive(
    pane: &Pane,
    mut sub: Receiver<MutationEvent>,
    cond: WaitCondition,
    deadline: Instant,
) -> Outcome {
    // Pre-check: many conditions can be satisfied immediately.
    if let Some(seq) = immediate_match(pane, &cond) {
        return Outcome::Matched { sequence: seq };
    }

    // For idle / cursor-stable we need a per-iteration timer that resets on
    // each event.
    let mut quiet_deadline = next_quiet_deadline(&cond);

    loop {
        let next_wake = quiet_deadline.map_or(deadline, |q| q.min(deadline));
        let remaining = next_wake.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            // Either the user-timeout fired or the quiet-period elapsed.
            if quiet_deadline.is_some_and(|q| q <= deadline) {
                // Quiet-period satisfied — for `idle`/`cursor_stable` this is
                // a successful match.
                let seq = pane.engine.snapshot().sequence;
                return Outcome::Matched { sequence: seq };
            }
            return Outcome::Timeout;
        }

        // For `Exit`, poll the child periodically alongside event waits.
        let exit_tick = if matches!(cond, WaitCondition::Exit) {
            Some(remaining.min(Duration::from_millis(50)))
        } else {
            None
        };
        let select_timeout = exit_tick.unwrap_or(remaining);

        match tokio::time::timeout(select_timeout, sub.recv()).await {
            Ok(Ok(_evt)) => {
                if let Some(seq) = immediate_match(pane, &cond) {
                    return Outcome::Matched { sequence: seq };
                }
                quiet_deadline = next_quiet_deadline(&cond);
            }
            Ok(Err(RecvError::Lagged(_))) => {
                // Missed events under load — re-check.
                if let Some(seq) = immediate_match(pane, &cond) {
                    return Outcome::Matched { sequence: seq };
                }
            }
            Ok(Err(RecvError::Closed)) => return Outcome::PaneDead,
            Err(_) => {
                // Tick timeout (quiet period or exit poll). For Exit, poll.
                if matches!(cond, WaitCondition::Exit)
                    && pane.pty.try_exit_code().ok().flatten().is_some()
                {
                    let seq = pane.engine.snapshot().sequence;
                    return Outcome::Matched { sequence: seq };
                }
                // For quiet-period conditions, the tick is the answer.
                if quiet_deadline.is_some()
                    && quiet_deadline.is_some_and(|q| q <= Instant::now())
                    && let Some(seq) = quiet_match(pane, &cond)
                {
                    return Outcome::Matched { sequence: seq };
                }
            }
        }
    }
}

/// Synchronous check: is the condition already satisfied?
fn immediate_match(pane: &Pane, cond: &WaitCondition) -> Option<u64> {
    let snap = pane.engine.snapshot();
    match cond {
        WaitCondition::Since { since } => (snap.sequence > *since).then_some(snap.sequence),
        WaitCondition::Text { regex } => {
            let body = visible_text(&snap);
            Regex::new(regex)
                .ok()
                .and_then(|re| re.is_match(&body).then_some(snap.sequence))
        }
        WaitCondition::AltScreen { on } => (snap.modes.alt_screen == *on).then_some(snap.sequence),
        // Hash is resolved to `Since` upstream; Cells/Idle/CursorStable/Exit
        // are event-driven below.
        WaitCondition::Hash { .. }
        | WaitCondition::Cells { .. }
        | WaitCondition::CursorStable { .. }
        | WaitCondition::Idle { .. }
        | WaitCondition::Exit => None,
    }
}

/// For idle / cursor-stable, return the next deadline at which the quiet
/// period would be considered satisfied if no further events arrive.
fn next_quiet_deadline(cond: &WaitCondition) -> Option<Instant> {
    match cond {
        WaitCondition::Idle { quiet_ms }
        | WaitCondition::CursorStable {
            stable_ms: quiet_ms,
        } => Some(Instant::now() + Duration::from_millis(*quiet_ms)),
        _ => None,
    }
}

fn quiet_match(pane: &Pane, cond: &WaitCondition) -> Option<u64> {
    match cond {
        WaitCondition::Idle { .. } | WaitCondition::CursorStable { .. } => {
            Some(pane.engine.snapshot().sequence)
        }
        _ => None,
    }
}

fn finish(pane: &Pane, outcome: &Outcome) -> Response {
    let snap = pane.engine.snapshot();
    match outcome {
        Outcome::Matched { sequence } => Response::ok(serde_json::json!({
            "pane": pane.id,
            "sequence": sequence,
            "hash": snap.canonical_hash(),
            "alt_screen": snap.modes.alt_screen,
        })),
        Outcome::Timeout => Response::err(ErrorBody::new(
            ErrorCode::WaitTimeout,
            "wait condition not satisfied within --max",
            "increase --max or revisit the predicate",
        )),
        Outcome::PaneDead => Response::err(ErrorBody::new(
            ErrorCode::PaneDead,
            "pane's PTY child exited mid-wait",
            "call list to confirm; spawn a fresh pane if needed",
        )),
    }
}

fn visible_text(snap: &agent_tui_engine::EngineSnapshot) -> String {
    let cols = usize::from(snap.grid.cols);
    let rows = usize::from(snap.grid.rows);
    let mut out = String::with_capacity(cols * rows);
    for row in 0..rows {
        for col in 0..cols {
            let cell = &snap.grid.cells[row * cols + col];
            if cell.width == 0 {
                continue;
            }
            out.push_str(&cell.ch);
        }
        out.push('\n');
    }
    out
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
