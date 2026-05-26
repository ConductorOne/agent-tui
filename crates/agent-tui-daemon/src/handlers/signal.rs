//! `signal` command handler.
//!
//! Delivers a UNIX signal to the foreground process group of the pane's
//! child. We deliver to the process group (via `killpg`) rather than the
//! direct pid so signals reach forwarded shell children too.

use std::sync::Arc;

use agent_tui_protocol::{ErrorBody, ErrorCode, PaneId, Response};

use crate::pane::{Pane, Registry, resolve_focused};

/// Send `signal` (e.g. `SIGTERM`, `SIGINT`, `2`, `15`) to the pane's
/// foreground process group.
pub async fn run(registry: &Arc<Registry>, pane: Option<PaneId>, signal: String) -> Response {
    let sig = match parse_signal(&signal) {
        Ok(s) => s,
        Err(reason) => {
            return Response::err(ErrorBody::new(
                ErrorCode::InvalidArgs,
                reason,
                "use a name like SIGINT/SIGTERM or a small positive integer",
            ));
        }
    };
    let pane_arc = match resolve_focused(registry, pane).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if let Err(e) = deliver(&pane_arc, sig) {
        return Response::err(ErrorBody::new(
            ErrorCode::Internal,
            e,
            "child may have exited; call list",
        ));
    }
    Response::ok(serde_json::json!({
        "pane": pane_arc.id,
        "signal": signal,
    }))
}

#[cfg(unix)]
fn deliver(pane: &Pane, sig: nix::sys::signal::Signal) -> Result<(), String> {
    use nix::sys::signal::killpg;
    use nix::unistd::Pid;
    let pgid = pane
        .pty
        .pgid()
        .ok_or_else(|| "pane has no process group (child may have exited)".to_string())?;
    killpg(Pid::from_raw(pgid), sig).map_err(|e| format!("killpg failed: {e}"))
}

#[cfg(not(unix))]
fn deliver(_pane: &Pane, _sig: i32) -> Result<(), String> {
    Err("signal delivery is unix-only in v0.1.0".into())
}

#[cfg(unix)]
fn parse_signal(s: &str) -> Result<nix::sys::signal::Signal, String> {
    use nix::sys::signal::Signal;
    let trimmed = s.trim();
    if let Ok(n) = trimmed.parse::<i32>() {
        return Signal::try_from(n).map_err(|_| format!("unknown signal number {n}"));
    }
    let upper = trimmed.to_ascii_uppercase();
    let normalized = upper.strip_prefix("SIG").unwrap_or(&upper);
    match normalized {
        "HUP" => Ok(Signal::SIGHUP),
        "INT" => Ok(Signal::SIGINT),
        "QUIT" => Ok(Signal::SIGQUIT),
        "ABRT" => Ok(Signal::SIGABRT),
        "KILL" => Ok(Signal::SIGKILL),
        "TERM" => Ok(Signal::SIGTERM),
        "USR1" => Ok(Signal::SIGUSR1),
        "USR2" => Ok(Signal::SIGUSR2),
        "WINCH" => Ok(Signal::SIGWINCH),
        "STOP" => Ok(Signal::SIGSTOP),
        "CONT" => Ok(Signal::SIGCONT),
        other => Err(format!("unknown signal name {other}")),
    }
}

#[cfg(not(unix))]
fn parse_signal(_s: &str) -> Result<i32, String> {
    Err("signal delivery is unix-only in v0.1.0".into())
}
