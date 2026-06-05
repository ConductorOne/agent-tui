//! `die` command handler.
//!
//! Tears a pane down **group-aware**: the teardown signal goes to the
//! child's process group (via the same `killpg` path `signal` uses), not
//! just the child PID. A harness like `claude` forks MCP servers and tool
//! subprocesses into that group; a child-PID-only kill (the old behavior,
//! one best-effort signal through portable-pty's `ChildKiller`) left them
//! running as orphans. Sending to the group reaps them too.
//!
//! Two modes:
//!  - Plain `die` — one SIGTERM to the group, immediate, no wait.
//!  - `die --grace <ms>` — SIGTERM the group, poll for exit up to `<ms>`,
//!    then SIGKILL the group if anything is still alive.
//!
//! Either way the registry entry is dropped, the reader task aborts on
//! drop, and subsequent `list` calls no longer report the pane.

use std::sync::Arc;
use std::time::Duration;

use agent_tui_protocol::{ErrorBody, ErrorCode, PaneId, Response};

use crate::pane::{Pane, Registry, resolve_focused};

/// Resolve the target pane, tear down its process group, and drop the
/// registry entry. Clears focus if the killed pane was the focused one.
pub async fn run(
    registry: &Arc<Registry>,
    pane: Option<PaneId>,
    grace: Option<Duration>,
) -> Response {
    let id = match resolve_focused(registry, pane).await {
        Ok(p) => p.id.clone(),
        Err(resp) => return resp,
    };

    let Some(entry) = registry.remove(&id).await else {
        return Response::err(ErrorBody::new(
            ErrorCode::NoActivePane,
            format!("pane {id} not found"),
            "call list to see live panes",
        ));
    };

    // Demote focus to `Held` when the focused pane dies. Future no-`--pane`
    // commands error until the user re-focuses explicitly.
    registry.mark_focus_held_if(&id).await;

    let outcome = teardown(&entry, grace).await;

    Response::ok(serde_json::json!({
        "pane": id,
        // Best-effort: a delivery error usually just means the child had
        // already exited, which is still a successful teardown.
        "killed": outcome.error.is_none(),
        "grace_ms": grace.map(|g| u64::try_from(g.as_millis()).unwrap_or(u64::MAX)),
        // True when the grace window elapsed and we had to SIGKILL the group.
        "escalated": outcome.escalated,
        "kill_error": outcome.error,
    }))
}

/// Result of a teardown attempt.
struct Teardown {
    /// SIGKILL escalation was required (the group outlived the grace window).
    escalated: bool,
    /// Failure reason if the teardown signal couldn't be delivered (e.g. the
    /// child had already exited). Best-effort; treated as success-ish.
    error: Option<String>,
}

/// Unix teardown: SIGTERM the process group, optionally wait for the whole
/// group to drain, then SIGKILL the group if it outlives the grace window.
#[cfg(unix)]
async fn teardown(entry: &Pane, grace: Option<Duration>) -> Teardown {
    use nix::sys::signal::Signal;

    use super::signal::killpg_pgid;

    // Capture the process-group id *before* signaling. Once the group leader
    // (the pane's direct child) exits, the tty's foreground-pgrp query becomes
    // unreliable, but the captured id stays valid for `killpg` as long as any
    // group member is alive — which is exactly the orphan case we must reap.
    let Some(pgid) = entry.pty.pgid() else {
        return Teardown {
            escalated: false,
            error: Some("pane has no process group (child may have exited)".into()),
        };
    };

    // SIGTERM the whole group — not just the child PID — so the harness's
    // forked MCP servers / tool subprocesses are reaped too, not orphaned.
    let term = killpg_pgid(pgid, Signal::SIGTERM);

    let Some(grace) = grace else {
        // Immediate path: one SIGTERM to the group, no wait, no escalation.
        return Teardown {
            escalated: false,
            error: term.err(),
        };
    };

    // Couldn't even deliver SIGTERM (group already gone) → nothing to wait on.
    if let Err(e) = term {
        return Teardown {
            escalated: false,
            error: Some(e),
        };
    }

    // Poll until the *group* drains within the grace window. The direct child
    // may exit on SIGTERM while a forked grandchild lingers (the orphan
    // hazard), so we track group liveness — not just the child's own exit.
    if wait_for_group_exit(pgid, grace).await {
        return Teardown {
            escalated: false,
            error: None,
        };
    }

    // Group still has members after the grace window → SIGKILL the group.
    let kill = killpg_pgid(pgid, Signal::SIGKILL);
    Teardown {
        escalated: true,
        error: kill.err(),
    }
}

/// Poll the process group for emptiness until `grace` elapses. Returns `true`
/// once the group has drained, `false` if it still has members when the
/// window closes. Bounded by `grace`; no sleeps beyond it.
#[cfg(unix)]
async fn wait_for_group_exit(pgid: i32, grace: Duration) -> bool {
    use tokio::time::{Instant, sleep};

    use super::signal::group_alive;

    const POLL: Duration = Duration::from_millis(20);
    let deadline = Instant::now() + grace;
    loop {
        if !group_alive(pgid) {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        sleep(POLL.min(deadline - now)).await;
    }
}

/// Windows teardown: portable-pty has no `killpg`, so we terminate the child
/// via its `ChildKiller` (`TerminateProcess`). With `grace`, give the child a
/// chance to exit on its own first; without it, terminate immediately (the
/// historical behavior).
#[cfg(windows)]
async fn teardown(entry: &Pane, grace: Option<Duration>) -> Teardown {
    use tokio::time::{Instant, sleep};

    if let Some(grace) = grace {
        const POLL: Duration = Duration::from_millis(20);
        let deadline = Instant::now() + grace;
        loop {
            match entry.pty.try_exit_code() {
                Ok(Some(_)) | Err(_) => {
                    return Teardown {
                        escalated: false,
                        error: None,
                    };
                }
                Ok(None) => {}
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            sleep(POLL.min(deadline - now)).await;
        }
    }
    let error = entry.pty.kill().err().map(|e| e.to_string());
    Teardown {
        escalated: grace.is_some(),
        error,
    }
}
