//! Per-program adapter trait and the language-agnostic plug-in IPC.
//!
//! Built-in adapters live alongside the daemon. Third-party adapters are
//! sub-processes speaking **JSON-RPC over stdio** (the language-neutral
//! plug-in protocol from `docs/RFC.md` §9.1).

#![forbid(unsafe_code)]

use agent_tui_engine::EngineSnapshot;
use agent_tui_protocol::Outline;
use serde::{Deserialize, Serialize};

/// Adapter errors.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// Adapter refused the request (e.g., wrong program identity).
    #[error("refused: {0}")]
    Refused(String),
    /// Adapter RPC layer failed (sub-process died, timeout, parse error).
    #[error("rpc: {0}")]
    Rpc(String),
    /// Underlying IO failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Information about a pane passed to `Adapter::detect`.
///
/// Adapters use this to score a confidence (0.0..1.0).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneInfo {
    /// argv used to spawn the pane's child.
    pub argv: Vec<String>,
    /// Resolved comm (basename of argv[0] or /proc/<pid>/comm).
    pub comm: String,
    /// First 512 bytes of child output, if any.
    pub first_bytes: Vec<u8>,
    /// Environment variables visible at spawn (filtered for secrets).
    pub env: std::collections::BTreeMap<String, String>,
}

/// Capability descriptor returned from `Adapter::initialize`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    /// Whether `eval` is supported.
    pub supports_eval: bool,
    /// Whether the adapter pushes events asynchronously (independent of
    /// snapshot calls).
    pub supports_streaming_events: bool,
    /// Human-readable adapter version (semver-ish).
    pub version: String,
}

/// Async notification emitted by an adapter outside the normal `Outline`
/// request/response cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "variant", rename_all = "snake_case")]
pub enum Notification {
    /// Generic event the adapter wants the firehose to know about.
    Event {
        /// Event kind tag.
        kind: String,
        /// Event payload (adapter-specific).
        data: serde_json::Value,
    },
    /// Adapter believes the program identity changed; daemon should re-run
    /// `Adapter::detect`.
    Detect {
        /// New confidence the adapter would return today.
        confidence: f32,
    },
    /// Adapter is now in degraded mode (lost RPC peer, etc.).
    Degraded {
        /// Reason string for the audit log.
        reason: String,
    },
}

/// The per-program adapter trait.
///
/// Built-in adapters implement this directly. The plug-in IPC wrapper
/// (`PluginAdapter`, lands in P2) implements `Adapter` by speaking JSON-RPC
/// over a sub-process's stdin/stdout. From the daemon's POV the two are
/// indistinguishable.
#[async_trait::async_trait]
pub trait Adapter: Send + Sync {
    /// Adapter registry key (`generic`, `nvim`, `tmux`, ...).
    fn name(&self) -> &'static str;

    /// Inspect pane info and return confidence in `[0.0, 1.0]`.
    async fn detect(&self, info: &PaneInfo) -> f32;

    /// Called once on attach. The adapter may e.g. open a Unix socket to
    /// the program's RPC endpoint.
    async fn initialize(&self) -> Result<Capabilities, AdapterError>;

    /// Build the structured outline for the given engine snapshot.
    ///
    /// MUST be re-entrant and side-effect-free.
    async fn outline(&self, snap: &EngineSnapshot) -> Result<Outline, AdapterError>;

    /// Execute an `eval` expression. Only required when
    /// [`Capabilities::supports_eval`] is true.
    async fn eval(&self, expr: &str) -> Result<serde_json::Value, AdapterError>;

    /// Release any resources held by the adapter.
    async fn shutdown(&self) -> Result<(), AdapterError>;
}

/// A minimal built-in `generic` adapter — confidence 0.1 for everything,
/// outline-only with empty content. Real outline heuristics land in P0.
pub struct GenericAdapter;

#[async_trait::async_trait]
impl Adapter for GenericAdapter {
    fn name(&self) -> &'static str {
        "generic"
    }

    async fn detect(&self, _info: &PaneInfo) -> f32 {
        0.1
    }

    async fn initialize(&self) -> Result<Capabilities, AdapterError> {
        Ok(Capabilities {
            supports_eval: false,
            supports_streaming_events: false,
            version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }

    async fn outline(&self, _snap: &EngineSnapshot) -> Result<Outline, AdapterError> {
        Ok(Outline {
            adapter: "generic".to_string(),
            nodes: Vec::new(),
        })
    }

    async fn eval(&self, _expr: &str) -> Result<serde_json::Value, AdapterError> {
        Err(AdapterError::Refused(
            "generic adapter does not support eval".to_string(),
        ))
    }

    async fn shutdown(&self) -> Result<(), AdapterError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_tui_engine::{Cell, CellGrid, EngineSnapshot, ModeFlags};

    #[tokio::test]
    async fn generic_returns_empty_outline() {
        let a = GenericAdapter;
        let snap = EngineSnapshot {
            grid: CellGrid {
                cols: 1,
                rows: 1,
                cells: vec![Cell {
                    ch: " ".into(),
                    width: 1,
                    fg: 0,
                    bg: 0,
                    attrs: 0,
                }],
                cursor: (0, 0),
            },
            modes: ModeFlags::default(),
            sequence: 0,
        };
        let outline = a.outline(&snap).await.expect("outline");
        assert_eq!(outline.adapter, "generic");
        assert!(outline.nodes.is_empty());
    }
}
