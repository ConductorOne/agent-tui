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

#[cfg(feature = "bwrap")]
pub mod bwrap;

#[cfg(feature = "bwrap")]
pub mod fake_inference;

/// Wrapper around a snapshot response with the assertion helpers.
///
/// Lives in `lib.rs` so both the Docker backend (`scenario.rs`) and the
/// bwrap backend (`bwrap.rs`) can build them without one depending on
/// the other.
pub struct Snapshot {
    envelope: serde_json::Value,
}

impl Snapshot {
    /// Build from the `data` payload of an agent-tui snapshot response.
    #[must_use]
    pub fn from_envelope(envelope: serde_json::Value) -> Self {
        Self { envelope }
    }

    /// Whole snapshot envelope (`data` field of the agent-tui response).
    #[must_use]
    pub fn envelope(&self) -> &serde_json::Value {
        &self.envelope
    }

    fn outline_text(&self) -> String {
        // Walk the full outline tree (nodes + nested children) — the
        // addressing-model adapters emit a single root with content
        // under `children`, so a top-level-only walk would miss
        // everything. Concatenate every node's `name` with newlines.
        fn walk(node: &serde_json::Value, out: &mut String) {
            if let Some(name) = node.get("name").and_then(serde_json::Value::as_str) {
                if !name.is_empty() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(name);
                }
            }
            if let Some(kids) = node.get("children").and_then(serde_json::Value::as_array) {
                for k in kids {
                    walk(k, out);
                }
            }
        }
        let nodes = self.envelope.get("outline").and_then(|o| o.get("nodes"));
        let mut out = String::new();
        if let Some(arr) = nodes.and_then(serde_json::Value::as_array) {
            for n in arr {
                walk(n, &mut out);
            }
        }
        out
    }

    /// Return the first node whose ref equals `target_ref`, walking
    /// the outline tree recursively. Cheap and addressable from tests
    /// (`snap.find("@vim.statusline").unwrap().get("name")`).
    #[must_use]
    pub fn find(&self, target_ref: &str) -> Option<&serde_json::Value> {
        fn walk<'a>(node: &'a serde_json::Value, want: &str) -> Option<&'a serde_json::Value> {
            if node.get("ref").and_then(serde_json::Value::as_str) == Some(want) {
                return Some(node);
            }
            if let Some(kids) = node.get("children").and_then(serde_json::Value::as_array) {
                for k in kids {
                    if let Some(found) = walk(k, want) {
                        return Some(found);
                    }
                }
            }
            None
        }
        let nodes = self.envelope.get("outline")?.get("nodes")?.as_array()?;
        nodes.iter().find_map(|n| walk(n, target_ref))
    }

    /// Assert that the rendered outline text contains `needle`. Errors
    /// (with the full outline) when it doesn't; tests typically `?` the
    /// result so a panic routes through the `Drop` artifact-capture path.
    pub fn assert_outline_contains(&self, needle: &str) -> Result<()> {
        let body = self.outline_text();
        if body.contains(needle) {
            Ok(())
        } else {
            anyhow::bail!("outline does not contain {needle:?}; full outline:\n---\n{body}\n---")
        }
    }

    /// `state` field (`shell` / `running` / `alt_screen_tui` / …).
    pub fn state(&self) -> Option<&str> {
        self.envelope
            .get("state")
            .and_then(serde_json::Value::as_str)
    }
}

/// Per-test artifact directory under `target/integration-artifacts/<name>/`.
///
/// `Drop`-time captures are written into this directory **only on panic**
/// — successful tests don't pollute the dir.
///
/// On a panic the dump produces:
///
/// ```text
/// target/integration-artifacts/<scenario>/
///   README.md          Human-readable index + playback instructions.
///   meta.json          Scenario name, image, container id, panic message.
///   command-log.json   Every agent-tui CLI call (op, args, response, ms).
///   snapshots/         All snapshots taken in order: 001.json, 002.json, …
///   pane.cast          asciicast pulled from inside the container so the
///                      whole TUI stream can be replayed via `asciinema
///                      play pane.cast`. Best-effort: skipped silently if
///                      the cast file isn't present (no pane spawned yet).
///   container.log      `docker logs <container>` output. Best-effort.
/// ```
pub struct ArtifactDir {
    root: PathBuf,
    /// Each (op, response) the scenario has sent so far. Persisted on panic.
    log: Mutex<Vec<CommandRecord>>,
    /// Every snapshot the scenario has taken, in order.
    snapshot_history: Mutex<Vec<serde_json::Value>>,
    /// Static scenario metadata captured at construction time.
    meta: Mutex<ScenarioMeta>,
}

/// Static metadata about the running scenario, dumped to `meta.json`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ScenarioMeta {
    /// Scenario name (matches the directory under `target/integration-artifacts/`).
    pub name: String,
    /// Container image tag used.
    pub image: String,
    /// Container id once started.
    pub container_id: Option<String>,
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
    pub fn new(test_name: &str, image: &str) -> Result<Self> {
        let root = workspace_root()?
            .join("target")
            .join("integration-artifacts")
            .join(sanitize(test_name));
        std::fs::create_dir_all(&root)
            .with_context(|| format!("create artifact dir {}", root.display()))?;
        std::fs::create_dir_all(root.join("snapshots"))?;
        Ok(Self {
            root,
            log: Mutex::new(Vec::new()),
            snapshot_history: Mutex::new(Vec::new()),
            meta: Mutex::new(ScenarioMeta {
                name: test_name.to_string(),
                image: image.to_string(),
                container_id: None,
            }),
        })
    }

    /// Append a command + response record.
    pub fn record(&self, record: CommandRecord) {
        if let Ok(mut log) = self.log.lock() {
            log.push(record);
        }
    }

    /// Append a snapshot to the history.
    pub fn push_snapshot(&self, snap: serde_json::Value) {
        if let Ok(mut g) = self.snapshot_history.lock() {
            g.push(snap);
        }
    }

    /// Set the container id once the container has started.
    pub fn set_container_id(&self, id: &str) {
        if let Ok(mut m) = self.meta.lock() {
            m.container_id = Some(id.to_string());
        }
    }

    /// Root directory under `target/`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Snapshot the metadata for callers that need it (e.g. the `docker
    /// exec` helper in `Scenario::dump_blocking`).
    pub fn meta_snapshot(&self) -> ScenarioMeta {
        self.meta.lock().map(|m| m.clone()).unwrap_or_default()
    }

    /// Write the synchronous artifacts (everything that lives in memory).
    /// Called automatically on panic by `Scenario::drop` and explicitly
    /// by `Scenario::capture_now` for debugging passing tests.
    ///
    /// The async-fetched bits (cast file + container logs) live in
    /// `Scenario::dump_diagnostics_blocking` which shells out to `docker`.
    pub fn dump(&self) -> std::io::Result<()> {
        if let Ok(log) = self.log.lock() {
            let json = serde_json::to_string_pretty(&*log).unwrap_or_default();
            std::fs::write(self.root.join("command-log.json"), json)?;
        }
        if let Ok(history) = self.snapshot_history.lock() {
            for (i, snap) in history.iter().enumerate() {
                let path = self
                    .root
                    .join("snapshots")
                    .join(format!("{:03}.json", i + 1));
                let json = serde_json::to_string_pretty(snap).unwrap_or_default();
                std::fs::write(path, json)?;
            }
        }
        if let Ok(meta) = self.meta.lock() {
            let json = serde_json::to_string_pretty(&*meta).unwrap_or_default();
            std::fs::write(self.root.join("meta.json"), json)?;
        }
        self.write_readme()?;
        Ok(())
    }

    fn write_readme(&self) -> std::io::Result<()> {
        let meta = self.meta.lock().ok();
        let name = meta
            .as_ref()
            .map_or_else(|| "<unknown>".into(), |m| m.name.clone());
        let image = meta
            .as_ref()
            .map_or_else(|| "<unknown>".into(), |m| m.image.clone());
        let cid = meta
            .as_ref()
            .and_then(|m| m.container_id.clone())
            .unwrap_or_else(|| "<container did not start>".into());
        let body = format!(
            r"# Integration test artifacts: {name}

**Image:** `{image}`
**Container ID:** `{cid}`

## How to debug

1. **Replay the agent's view:** the `pane.cast` file is an asciicast
   captured from the daemon's recorder *inside the container*. It contains
   every PTY byte the TUI emitted with original timing.

   ```bash
   asciinema play pane.cast
   ```

2. **Look at the screenshot:** `last-snapshot.png` is a rasterized
   render of the final pane state with `@eN` ref labels overlaid (the
   `--annotate` mode). Useful for grok-at-a-glance failure visualization
   without needing asciinema installed.

3. **See state evolution:** every snapshot the scenario took lives in
   `snapshots/NNN.json` in chronological order. Diff consecutive
   snapshots to spot where state went wrong.

4. **See every CLI call:** `command-log.json` has the agent-tui CLI op,
   its args, the daemon response, and elapsed-ms-since-scenario-start
   for every call.

5. **Read container logs:** `container.log` is `docker logs <container>`
   captured at panic time. Usually just the long-running entrypoint's
   output (PTY traffic flows through agent-tui, not stdout, so this is
   often empty for TUI scenarios).

## Files

| File | What it is |
|------|------------|
| `README.md` | This index. |
| `meta.json` | Scenario name + image + container id, JSON. |
| `command-log.json` | Every agent-tui CLI op + response, chronological. |
| `snapshots/NNN.json` | Snapshots in capture order. |
| `pane.cast` | asciicast — replay via `asciinema play`. Best-effort. |
| `last-snapshot.png` | Annotated PNG render of the final pane state. Best-effort. |
| `container.log` | `docker logs` stdout. Best-effort. |
| `container.err` | `docker logs` stderr. Best-effort. |
"
        );
        std::fs::write(self.root.join("README.md"), body)
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
    // `AGENT_TUI_BINARY` overrides path discovery — used by CI to
    // point at a musl-static binary built under
    // `target/x86_64-unknown-linux-musl/debug/`, which is glibc-
    // version-independent and runs cleanly inside a debian-slim
    // container.
    if let Some(p) = std::env::var_os("AGENT_TUI_BINARY") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Ok(p);
        }
        anyhow::bail!(
            "AGENT_TUI_BINARY points at {} but the file doesn't exist",
            p.display()
        );
    }
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
