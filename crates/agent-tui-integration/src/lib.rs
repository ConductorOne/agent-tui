//! Real-world TUI integration tests for `agent-tui`.
//!
//! Each test spins up a hermetic Docker (or Podman) container, copies the
//! freshly-built `agent-tui` binary into it, exercises a real TUI inside,
//! and asserts on the snapshot outputs. On panic, a `Drop` hook on
//! [`Scenario`] writes diagnostic artifacts (command log, last response,
//! last snapshot) to `target/integration-artifacts/<test>/` so the
//! failure is debuggable from a CI log alone.
//!
//! Tests live under `tests/` and are gated behind the `docker` Cargo
//! feature — bare `cargo test --workspace` will skip them. CI runs
//! `cargo test -p agent-tui-integration --features docker` against
//! whichever Docker-API endpoint the host advertises via `DOCKER_HOST`
//! (Docker, Podman, or `colima`).
//!
//! See `docs/research/testcontainers-spike.md` for the design.

#![forbid(unsafe_code)]
#![allow(dead_code)] // I1 ships scaffolding; later cycles wire it up.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::Serialize;

#[cfg(feature = "docker")]
pub mod scenario;

/// Per-test artifact directory under `target/integration-artifacts/<name>/`.
///
/// `Drop`-time captures are written into this directory **only on panic**
/// — successful tests don't pollute the dir.
pub struct ArtifactDir {
    root: PathBuf,
    /// Each (op, response) the scenario has sent so far. Persisted on panic.
    log: Mutex<Vec<CommandRecord>>,
    /// Latest snapshot envelope, for the panic-time dump.
    last_snapshot: Mutex<Option<serde_json::Value>>,
}

/// One entry in the per-scenario command log captured for debugging.
#[derive(Debug, Clone, Serialize)]
pub struct CommandRecord {
    /// Human-readable op name (`spawn`, `press`, `wait`, ...).
    pub op: String,
    /// Full args / payload as JSON.
    pub args: serde_json::Value,
    /// Daemon response envelope (success or error).
    pub response: serde_json::Value,
    /// Elapsed wall-clock since scenario start, milliseconds.
    pub elapsed_ms: u128,
}

impl ArtifactDir {
    /// Open the artifact directory for `<test_name>`. Creates it on demand.
    pub fn new(test_name: &str) -> Result<Self> {
        let root = workspace_root()?
            .join("target")
            .join("integration-artifacts")
            .join(sanitize(test_name));
        std::fs::create_dir_all(&root)
            .with_context(|| format!("create artifact dir {}", root.display()))?;
        Ok(Self {
            root,
            log: Mutex::new(Vec::new()),
            last_snapshot: Mutex::new(None),
        })
    }

    /// Append a command + response record.
    pub fn record(&self, record: CommandRecord) {
        if let Ok(mut log) = self.log.lock() {
            log.push(record);
        }
    }

    /// Remember the latest snapshot so panic-time capture can dump it.
    pub fn set_last_snapshot(&self, snap: serde_json::Value) {
        if let Ok(mut g) = self.last_snapshot.lock() {
            *g = Some(snap);
        }
    }

    /// Root directory under `target/`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Write artifacts to disk. Called automatically on panic by
    /// [`Scenario::drop`]; can also be called explicitly to capture
    /// state mid-scenario for debugging passing tests.
    pub fn dump(&self) -> std::io::Result<()> {
        if let Ok(log) = self.log.lock() {
            let json = serde_json::to_string_pretty(&*log).unwrap_or_default();
            std::fs::write(self.root.join("command-log.json"), json)?;
        }
        if let Ok(snap) = self.last_snapshot.lock()
            && let Some(s) = snap.as_ref()
        {
            let json = serde_json::to_string_pretty(s).unwrap_or_default();
            std::fs::write(self.root.join("last-snapshot.json"), json)?;
        }
        Ok(())
    }
}

/// Locate the workspace root by walking up from this crate's `Cargo.toml`.
pub fn workspace_root() -> Result<PathBuf> {
    // CARGO_MANIFEST_DIR is `<workspace>/crates/agent-tui-integration` at
    // build time; pop twice.
    let me = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    me.parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .context("CARGO_MANIFEST_DIR has no grandparent (broken layout?)")
}

/// Path to the `agent-tui` binary built by the workspace.
///
/// Prefers `target/debug/` (set during `cargo test`) over `target/release/`.
/// Returns an error if neither exists; CI must `cargo build --bin agent-tui`
/// (or `cargo build --release ...`) before running integration tests.
pub fn agent_tui_binary() -> Result<PathBuf> {
    let root = workspace_root()?;
    let exe = if cfg!(windows) {
        "agent-tui.exe"
    } else {
        "agent-tui"
    };
    let candidates = [
        root.join("target").join("debug").join(exe),
        root.join("target").join("release").join(exe),
    ];
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    anyhow::bail!(
        "agent-tui binary not found in {} — run `cargo build --bin agent-tui` first",
        candidates[0].display()
    )
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
