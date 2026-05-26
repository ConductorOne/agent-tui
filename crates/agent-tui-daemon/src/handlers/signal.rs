//! `signal` command handler.
//!
//! Delivers a UNIX-style signal to the pane's child:
//!  - Unix: `killpg` to the process group so forwarded shell children get hit.
//!  - Windows: `GenerateConsoleCtrlEvent` for SIGINT/SIGBREAK; `TerminateProcess`
//!    (via portable-pty's `ChildKiller`) for SIGTERM/SIGKILL. portable-pty
//!    spawns Windows children with `CREATE_NEW_PROCESS_GROUP` so the child PID
//!    is already a valid control-event group id.

use std::sync::Arc;

use agent_tui_protocol::{ErrorBody, ErrorCode, PaneId, Response};

use crate::pane::{Pane, Registry, resolve_focused};

/// Send `signal` (e.g. `SIGTERM`, `SIGINT`, `2`, `15`) to the pane's
/// foreground process group / Windows console group.
pub async fn run(registry: &Arc<Registry>, pane: Option<PaneId>, signal: String) -> Response {
    let pane_arc = match resolve_focused(registry, pane).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    match deliver(&pane_arc, &signal) {
        Ok(()) => Response::ok(serde_json::json!({
            "pane": pane_arc.id,
            "signal": signal,
        })),
        Err(DeliverErr::InvalidArgs(reason)) => Response::err(ErrorBody::new(
            ErrorCode::InvalidArgs,
            reason,
            "use a name like SIGINT/SIGTERM or a small positive integer",
        )),
        Err(DeliverErr::Internal(reason)) => Response::err(ErrorBody::new(
            ErrorCode::Internal,
            reason,
            "child may have exited; call list",
        )),
    }
}

enum DeliverErr {
    InvalidArgs(String),
    Internal(String),
}

#[cfg(unix)]
fn deliver(pane: &Pane, signal: &str) -> Result<(), DeliverErr> {
    use nix::sys::signal::killpg;
    use nix::unistd::Pid;
    let sig = parse_unix_signal(signal).map_err(DeliverErr::InvalidArgs)?;
    let pgid = pane.pty.pgid().ok_or_else(|| {
        DeliverErr::Internal("pane has no process group (child may have exited)".into())
    })?;
    killpg(Pid::from_raw(pgid), sig)
        .map_err(|e| DeliverErr::Internal(format!("killpg failed: {e}")))
}

#[cfg(unix)]
fn parse_unix_signal(s: &str) -> Result<nix::sys::signal::Signal, String> {
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

#[cfg(windows)]
fn deliver(pane: &Pane, signal: &str) -> Result<(), DeliverErr> {
    let kind = parse_win_signal(signal).map_err(DeliverErr::InvalidArgs)?;
    match kind {
        WinSig::CtrlC | WinSig::CtrlBreak => deliver_ctrl_event(pane, kind),
        WinSig::Terminate => deliver_terminate(pane),
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
enum WinSig {
    /// `SIGINT` / `2` → CTRL_C_EVENT.
    CtrlC,
    /// `SIGBREAK` / `SIGQUIT` → CTRL_BREAK_EVENT.
    CtrlBreak,
    /// `SIGTERM` / `SIGKILL` / `15` / `9` → `TerminateProcess` via ChildKiller.
    Terminate,
}

#[cfg(windows)]
fn parse_win_signal(s: &str) -> Result<WinSig, String> {
    let trimmed = s.trim();
    if let Ok(n) = trimmed.parse::<i32>() {
        return match n {
            2 => Ok(WinSig::CtrlC),
            3 => Ok(WinSig::CtrlBreak),
            9 | 15 => Ok(WinSig::Terminate),
            other => Err(format!(
                "signal number {other} is not mapped on Windows (only 2/3/9/15)"
            )),
        };
    }
    let upper = trimmed.to_ascii_uppercase();
    let normalized = upper.strip_prefix("SIG").unwrap_or(&upper);
    match normalized {
        "INT" => Ok(WinSig::CtrlC),
        "BREAK" | "QUIT" => Ok(WinSig::CtrlBreak),
        "TERM" | "KILL" => Ok(WinSig::Terminate),
        other => Err(format!(
            "signal {other} has no Windows analog (supported: INT, BREAK/QUIT, TERM, KILL)"
        )),
    }
}

#[cfg(windows)]
fn deliver_ctrl_event(pane: &Pane, kind: WinSig) -> Result<(), DeliverErr> {
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, GenerateConsoleCtrlEvent,
    };
    let pid = pane.pty.child_pid().ok_or_else(|| {
        DeliverErr::Internal("pane has no child PID (child may have exited)".into())
    })?;
    let event = match kind {
        WinSig::CtrlC => CTRL_C_EVENT,
        WinSig::CtrlBreak => CTRL_BREAK_EVENT,
        WinSig::Terminate => unreachable!("Terminate routed elsewhere"),
    };
    // SAFETY: GenerateConsoleCtrlEvent is a Win32 syscall; passing a known
    // event id + a u32 process-group id is the documented contract.
    #[allow(unsafe_code)]
    let ok = unsafe { GenerateConsoleCtrlEvent(event, pid) };
    if ok == 0 {
        return Err(DeliverErr::Internal(format!(
            "GenerateConsoleCtrlEvent failed for pid {pid}"
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn deliver_terminate(pane: &Pane) -> Result<(), DeliverErr> {
    // portable-pty's ChildKiller on Windows calls TerminateProcess; we already
    // have this exposed via PtyChild::kill().
    pane.pty
        .kill()
        .map_err(|e| DeliverErr::Internal(format!("TerminateProcess failed: {e}")))
}
