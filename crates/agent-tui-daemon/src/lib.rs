//! Long-lived per-session daemon for `agent-tui`.
//!
//! Speaks JSON-RPC over a Unix-domain socket — one line per request,
//! one line per response. The CLI lazily spawns the daemon if no live socket
//! is found at `$XDG_RUNTIME_DIR/agent-tui/<session>.sock`.
//!
//! Owns the per-session resources every handler needs: pane registry, the
//! seq→hash window backing `wait --hash`, snapshot generation tracker, and
//! the adapter registry consulted at spawn time. See `docs/design/RFC.md` §2, §4,
//! §5, §13.1.

// Crate-level `deny` rather than `forbid` so the Windows signal handler
// can opt into a single `unsafe` call (`GenerateConsoleCtrlEvent`, see
// handlers/signal.rs). Every other module stays unsafe-free.
#![deny(unsafe_code)]

pub mod adapter_registry;
pub mod classifier;
pub mod governance;
pub mod handlers;
pub mod hash_window;
pub mod osc133;
pub mod pane;
pub mod paths;
pub mod pty;
pub mod render;
pub mod server;
pub mod sidecar;
pub mod upgrade;

pub use paths::SocketLayout;
pub use server::{DaemonConfig, DaemonHandle, run_daemon};

/// Strip `HANDLE_FLAG_INHERIT` from this process's standard handles (Windows;
/// no-op elsewhere).
///
/// A CLI client calls this immediately before lazily spawning the DETACHED
/// daemon. Rust's std `Command::spawn` calls `CreateProcess` with
/// `bInheritHandles=TRUE`, so the daemon would otherwise inherit *every*
/// inheritable handle the client holds — including the client's stdout when it
/// is a pipe (`agent-tui run ... | consumer`). A held pipe write-end never EOFs,
/// so the consumer would block until the long-lived daemon exits. (File
/// redirection is unaffected: a file reads EOF regardless of extra open write
/// handles.) Clearing the inherit bit affects only child inheritance, not the
/// client's own use of the handles.
///
/// Lives here rather than in the CLI crate because the CLI is
/// `#![forbid(unsafe_code)]`; the single `SetHandleInformation` call is the
/// same `deny(unsafe_code)` opt-in used by the Windows signal path.
pub fn deny_std_handle_inheritance() {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation};
        for raw in [
            std::io::stdin().as_raw_handle(),
            std::io::stdout().as_raw_handle(),
            std::io::stderr().as_raw_handle(),
        ] {
            // Skip null / INVALID_HANDLE_VALUE (e.g. a detached std stream).
            if raw.is_null() || raw as isize == -1 {
                continue;
            }
            // SAFETY: `raw` is a valid open std handle for this process;
            // clearing the inherit bit is idempotent and cannot affect our own
            // use of the handle, only whether children inherit it.
            #[allow(unsafe_code)]
            unsafe {
                SetHandleInformation(raw as HANDLE, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}
