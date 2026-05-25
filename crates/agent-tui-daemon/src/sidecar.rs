//! Sidecar-file management.
//!
//! Sidecars carry small pieces of metadata the CLI reads without binding the
//! socket: the daemon's pid, version, selected engine, optional stream port,
//! optional metrics port. The version sidecar is what powers the
//! version-handshake check (RFC §4.6).

use std::path::Path;

use super::paths::SocketLayout;

/// Write the pid, version, and engine sidecars. Called once at daemon start.
///
/// # Errors
/// Filesystem errors from `std::fs::write`.
pub fn write_startup_sidecars(
    layout: &SocketLayout,
    binary_version: &str,
    engine: &str,
) -> std::io::Result<()> {
    let pid = std::process::id().to_string();
    std::fs::write(&layout.pid, pid)?;
    std::fs::write(&layout.version, binary_version)?;
    std::fs::write(&layout.engine, engine)?;
    Ok(())
}

/// Remove all sidecar files. Called on graceful shutdown.
///
/// Missing files are not errors — this is best-effort.
pub fn remove_all_sidecars(layout: &SocketLayout) {
    let _ = std::fs::remove_file(&layout.socket);
    let _ = std::fs::remove_file(&layout.pid);
    let _ = std::fs::remove_file(&layout.version);
    let _ = std::fs::remove_file(&layout.engine);
    let _ = std::fs::remove_file(&layout.stream);
    let _ = std::fs::remove_file(&layout.metrics);
}

/// Read the version sidecar, if present.
#[must_use]
pub fn read_version(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}
