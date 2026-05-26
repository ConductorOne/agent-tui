//! Socket-discovery hierarchy for the daemon (CLI ↔ daemon rendezvous).
//!
//! See `docs/RFC.md` §2.1: priority order is
//! `AGENT_TUI_SOCKET_DIR` → `XDG_RUNTIME_DIR` → `~/.agent-tui` → `tmpdir`.

use std::env;
use std::path::PathBuf;

use agent_tui_protocol::SessionId;
use interprocess::local_socket::Name;

/// Build the cross-platform `interprocess` socket name for a given layout.
///
/// On Unix we use a filesystem-path Unix socket (preserves the
/// `<session>.sock` file we discover via `SocketLayout`). On Windows we use
/// the namespaced API — named pipes — derived from the session basename so
/// the address is stable and discoverable.
///
/// # Errors
/// Returns the `io::Error` `interprocess` produces if the path/name can't
/// be encoded for the platform-native API.
pub fn socket_name(layout: &SocketLayout) -> std::io::Result<Name<'_>> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::{GenericFilePath, ToFsName};
        layout.socket.as_path().to_fs_name::<GenericFilePath>()
    }
    #[cfg(windows)]
    {
        use interprocess::local_socket::{GenericNamespaced, ToNsName};
        let stem = layout
            .socket
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("agent-tui");
        format!("agent-tui-{stem}").to_ns_name::<GenericNamespaced>()
    }
}

/// Where the daemon writes its socket and sidecar files.
///
/// All paths are computed deterministically from the session id + the
/// environment variables documented in the RFC. Resolving these once at
/// startup avoids subtle drift between CLI and daemon.
#[derive(Debug, Clone)]
pub struct SocketLayout {
    /// Root directory where all sidecar files live.
    pub root: PathBuf,
    /// Path to the Unix-domain socket the daemon binds.
    pub socket: PathBuf,
    /// `<session>.pid` file path.
    pub pid: PathBuf,
    /// `<session>.version` file path. Holds the binary's semver.
    pub version: PathBuf,
    /// `<session>.engine` file path. Holds `wezterm` or `alacritty`.
    pub engine: PathBuf,
    /// `<session>.stream` file path — port of the live-preview WebSocket
    /// server when enabled.
    pub stream: PathBuf,
    /// `<session>.metrics` file path — port of the Prometheus endpoint
    /// when enabled.
    pub metrics: PathBuf,
}

impl SocketLayout {
    /// Compute the layout for the given session id, observing the env
    /// hierarchy. Does **not** create any files — see [`SocketLayout::ensure_root`].
    #[must_use]
    pub fn for_session(session: &SessionId) -> Self {
        Self::for_session_in(session, Self::resolve_root())
    }

    /// Compute the layout for the given session id using an explicit root.
    /// Bypasses the env hierarchy. Useful for tests and CLI overrides.
    #[must_use]
    pub fn for_session_in(session: &SessionId, root: PathBuf) -> Self {
        let s = session.0.as_str();
        Self {
            socket: root.join(format!("{s}.sock")),
            pid: root.join(format!("{s}.pid")),
            version: root.join(format!("{s}.version")),
            engine: root.join(format!("{s}.engine")),
            stream: root.join(format!("{s}.stream")),
            metrics: root.join(format!("{s}.metrics")),
            root,
        }
    }

    /// Resolve the socket root following the RFC priority order:
    /// 1. `AGENT_TUI_SOCKET_DIR`
    /// 2. `XDG_RUNTIME_DIR`
    /// 3. `$HOME/.agent-tui`
    /// 4. system tmpdir
    fn resolve_root() -> PathBuf {
        if let Ok(d) = env::var("AGENT_TUI_SOCKET_DIR") {
            return PathBuf::from(d);
        }
        if let Ok(d) = env::var("XDG_RUNTIME_DIR") {
            return PathBuf::from(d).join("agent-tui");
        }
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home).join(".agent-tui");
        }
        env::temp_dir().join("agent-tui")
    }

    /// Create the root directory if missing. Idempotent.
    ///
    /// # Errors
    /// Filesystem errors from `create_dir_all`.
    pub fn ensure_root(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_paths_are_session_scoped() {
        let l = SocketLayout::for_session(&SessionId("hello".to_string()));
        assert!(l.socket.to_string_lossy().ends_with("hello.sock"));
        assert!(l.pid.to_string_lossy().ends_with("hello.pid"));
        assert!(l.version.to_string_lossy().ends_with("hello.version"));
    }

    // Note: the env-driven `explicit_socket_dir_wins` path is exercised by
    // integration tests under `tests/` (which can opt into `unsafe` env
    // mutation), not here — `#![forbid(unsafe_code)]` is non-negotiable for
    // the library crate.
}
