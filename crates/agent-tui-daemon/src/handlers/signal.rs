//! `signal` command handler.
//!
//! Delivers a UNIX-style signal to the pane's child:
//!  - Unix: `killpg` to the process group so forwarded shell children get hit.
//!  - Windows: SIGINT is delivered by writing ETX (`0x03`) into the
//!    pseudoconsole INPUT via the master writer — the idiomatic ConPTY approach
//!    used by Windows Terminal and wezterm (conhost translates the ETX byte into
//!    a real `CTRL_C_EVENT` for the pane's console clients). SIGBREAK / SIGQUIT
//!    (Ctrl-Break) is REJECTED with an honest error: no pseudoconsole input byte
//!    is translated into a `CTRL_BREAK_EVENT` (conhost has no line discipline),
//!    and `GenerateConsoleCtrlEvent` cannot reach the child — it runs on its own
//!    pseudoconsole and is not a process-group root (portable-pty does not set
//!    `CREATE_NEW_PROCESS_GROUP`). Writing FS `0x1c` (a former best-effort) is a
//!    no-op on Windows, so we no longer report success for it. SIGTERM / SIGKILL
//!    use the Windows descendant-tree kill in [`crate::pty::PtyChild::kill`].

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
    let sig = parse_unix_signal(signal).map_err(DeliverErr::InvalidArgs)?;
    signal_group(pane, sig).map_err(DeliverErr::Internal)
}

/// Deliver `sig` to the pane's foreground process group via `killpg`.
///
/// Both the `signal` handler and the group-aware `die` teardown go through
/// the `killpg` helpers below so the process-group reach is identical (and
/// `die` no longer orphans the harness's forked subprocesses the way a
/// child-PID-only kill did). Returns a human-readable reason on failure.
#[cfg(unix)]
pub(crate) fn signal_group(pane: &Pane, sig: nix::sys::signal::Signal) -> Result<(), String> {
    let pgid = pane
        .pty
        .pgid()
        .ok_or_else(|| "pane has no process group (child may have exited)".to_string())?;
    killpg_pgid(pgid, sig)
}

/// Deliver `sig` to process group `pgid` via `killpg`. Used by the `die`
/// teardown, which captures `pgid` once up front (the tty's foreground-pgrp
/// query becomes unreliable after the group leader exits, but the captured
/// id stays valid for `killpg` while any group member lives).
#[cfg(unix)]
pub(crate) fn killpg_pgid(pgid: i32, sig: nix::sys::signal::Signal) -> Result<(), String> {
    use nix::sys::signal::killpg;
    use nix::unistd::Pid;
    killpg(Pid::from_raw(pgid), sig).map_err(|e| format!("killpg failed: {e}"))
}

/// Whether process group `pgid` still has at least one live member, probed
/// with signal 0 (`killpg(pgid, None)` — an existence/permission check that
/// delivers nothing). `EPERM` still implies the group exists, so only `ESRCH`
/// counts as empty.
#[cfg(unix)]
pub(crate) fn group_alive(pgid: i32) -> bool {
    use nix::sys::signal::killpg;
    use nix::unistd::Pid;
    !matches!(
        killpg(Pid::from_raw(pgid), None),
        Err(nix::errno::Errno::ESRCH)
    )
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
        WinSig::CtrlC => deliver_interrupt(pane),
        // Ctrl-Break has no honest delivery path to a ConPTY child (see the
        // module doc): reject it rather than write a byte nothing consumes and
        // then falsely report success.
        WinSig::CtrlBreak => Err(DeliverErr::InvalidArgs(
            "SIGBREAK/SIGQUIT (Ctrl-Break) cannot be delivered to a pane child on \
             Windows: no pseudoconsole input byte is translated into a \
             CTRL_BREAK_EVENT, and GenerateConsoleCtrlEvent cannot reach a child \
             on its own pseudoconsole. Use `signal SIGINT` to interrupt, or \
             `signal SIGTERM` / `die` to stop the pane."
                .to_string(),
        )),
        WinSig::Terminate => deliver_terminate(pane),
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
enum WinSig {
    /// `SIGINT` / `2` → ETX (`0x03`) written to the ConPTY input.
    CtrlC,
    /// `SIGBREAK` / `SIGQUIT` → rejected on Windows with an honest error: no
    /// pseudoconsole-input byte is translated into a `CTRL_BREAK_EVENT`. Still
    /// parsed (so the name is recognized) but not deliverable.
    CtrlBreak,
    /// `SIGTERM` / `SIGKILL` / `15` / `9` → descendant-tree kill via `PtyChild::kill`.
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

/// Deliver SIGINT to the ConPTY child by writing ETX (`0x03`) to the
/// pseudoconsole INPUT via the existing master writer
/// ([`crate::pty::PtyChild::write_input`] locks the ConPTY input) — the idiomatic
/// ConPTY approach (how Windows Terminal and wezterm implement Ctrl-C): conhost
/// translates the ETX byte into a real `CTRL_C_EVENT` for the pane's console
/// clients. Used instead of `GenerateConsoleCtrlEvent`, which cannot reach the
/// child: it lives on a separate pseudoconsole and is not a process-group root
/// (portable-pty does not pass `CREATE_NEW_PROCESS_GROUP`), so the console-ctrl
/// call always fails and delivers nothing.
///
/// SIGBREAK/SIGQUIT is NOT handled here — [`deliver`] rejects it with an honest
/// error, because no pseudoconsole-input byte is translated into a
/// `CTRL_BREAK_EVENT` (writing FS `0x1c` would be a silent no-op).
///
/// Only interactive (`StdinMode::Pty`) panes have a ConPTY input to write to.
/// Headless run/ask/edit panes (`Pipe`/`Closed`) have `writer = io::sink()`, so
/// `write_input` would *succeed while discarding the byte* — reporting success
/// while delivering nothing. We reject those up front with an honest error
/// instead.
#[cfg(windows)]
fn deliver_interrupt(pane: &Pane) -> Result<(), DeliverErr> {
    use agent_tui_protocol::request::StdinMode;

    if pane.pty.stdin_mode() != StdinMode::Pty {
        return Err(DeliverErr::InvalidArgs(
            "cannot deliver a console interrupt (SIGINT) to a headless \
             run/ask/edit pane: it has no console input. Use `signal SIGTERM` \
             or `die` to stop it, or spawn an interactive pane."
                .to_string(),
        ));
    }
    pane.pty.write_input(&[0x03]).map_err(|e| {
        DeliverErr::Internal(format!("write interrupt byte to pseudoconsole input: {e}"))
    })
}

#[cfg(windows)]
fn deliver_terminate(pane: &Pane) -> Result<(), DeliverErr> {
    // portable-pty's ChildKiller on Windows calls TerminateProcess; we already
    // have this exposed via PtyChild::kill().
    pane.pty
        .kill()
        .map_err(|e| DeliverErr::Internal(format!("TerminateProcess failed: {e}")))
}
