//! Adapter registry + Detect lifecycle.
//!
//! Holds every built-in `Adapter` impl plus any externally-registered
//! plug-in adapters. On `spawn`, the registry runs `Adapter::detect` across
//! every registered adapter and returns the one with the highest confidence
//! score (subject to a minimum threshold — the `generic` fallback always
//! wins with confidence 0.1).
//!
//! Re-detection on alt-screen toggle / child-pid change / explicit rescan
//! lives in the daemon's spawn handler today; in P3 it migrates onto a
//! per-pane background task so adapters can switch under long-running
//! sessions.

use std::sync::Arc;

use agent_tui_adapter::{Adapter, ClaudeCodeAdapter, GenericAdapter, PaneInfo, ShellAdapter};

const MIN_CONFIDENCE: f32 = 0.05;

/// Read-only collection of attachable adapters.
#[derive(Clone, Default)]
pub struct AdapterRegistry {
    adapters: Vec<Arc<dyn Adapter>>,
}

impl AdapterRegistry {
    /// Build a registry pre-populated with the v1 built-ins:
    /// `generic` (fallback), `claude-code`, `shell`.
    #[must_use]
    pub fn with_builtins() -> Self {
        Self {
            adapters: vec![
                Arc::new(GenericAdapter),
                Arc::new(ClaudeCodeAdapter),
                Arc::new(ShellAdapter),
            ],
        }
    }

    /// Append an additional (built-in or plug-in) adapter.
    pub fn push(&mut self, adapter: Arc<dyn Adapter>) {
        self.adapters.push(adapter);
    }

    /// Run `detect` on every adapter; return the highest-confidence one.
    ///
    /// Ties break in registration order. Returns `None` only when no adapter
    /// scores above [`MIN_CONFIDENCE`] — in practice the `GenericAdapter`
    /// floor of 0.1 means there is always a winner.
    pub async fn detect_best(&self, info: &PaneInfo) -> Option<Arc<dyn Adapter>> {
        let mut best: Option<(f32, Arc<dyn Adapter>)> = None;
        for adapter in &self.adapters {
            let confidence = adapter.detect(info).await;
            if confidence < MIN_CONFIDENCE {
                continue;
            }
            match &best {
                Some((b, _)) if *b >= confidence => {}
                _ => best = Some((confidence, adapter.clone())),
            }
        }
        best.map(|(_, a)| a)
    }

    /// Number of adapters registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    /// Whether the registry has any adapters.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }

    /// Names of every registered adapter, in registration order.
    pub fn names(&self) -> Vec<String> {
        self.adapters.iter().map(|a| a.name().to_string()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_tui_adapter::{AdapterError, Capabilities};
    use agent_tui_engine::EngineSnapshot;
    use agent_tui_protocol::Outline;
    use async_trait::async_trait;

    /// Stub adapter for tests; reports a configured confidence.
    struct Stub {
        name: &'static str,
        confidence: f32,
    }

    #[async_trait]
    impl Adapter for Stub {
        fn name(&self) -> &'static str {
            self.name
        }
        async fn detect(&self, _info: &PaneInfo) -> f32 {
            self.confidence
        }
        async fn initialize(&self) -> Result<Capabilities, AdapterError> {
            Ok(Capabilities::default())
        }
        async fn outline(&self, _snap: &EngineSnapshot) -> Result<Outline, AdapterError> {
            Ok(Outline {
                adapter: self.name.to_string(),
                nodes: Vec::new(),
            })
        }
        async fn eval(&self, _expr: &str) -> Result<serde_json::Value, AdapterError> {
            Err(AdapterError::Refused("not supported".into()))
        }
        async fn shutdown(&self) -> Result<(), AdapterError> {
            Ok(())
        }
    }

    fn info() -> PaneInfo {
        PaneInfo {
            argv: vec!["bash".into()],
            comm: "bash".into(),
            first_bytes: Vec::new(),
            env: std::collections::BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn picks_highest_confidence() {
        let mut reg = AdapterRegistry::default();
        reg.push(Arc::new(Stub {
            name: "low",
            confidence: 0.2,
        }));
        reg.push(Arc::new(Stub {
            name: "high",
            confidence: 0.9,
        }));
        reg.push(Arc::new(Stub {
            name: "mid",
            confidence: 0.5,
        }));
        let picked = reg.detect_best(&info()).await.expect("some");
        assert_eq!(picked.name(), "high");
    }

    #[tokio::test]
    async fn rejects_below_min_threshold() {
        let mut reg = AdapterRegistry::default();
        reg.push(Arc::new(Stub {
            name: "noise",
            confidence: 0.01,
        }));
        assert!(reg.detect_best(&info()).await.is_none());
    }

    #[tokio::test]
    async fn builtins_detect_shell_for_bash() {
        // `with_builtins` ships shell/claude-code/generic. For a bash pane
        // ShellAdapter (0.85) outscores GenericAdapter (0.1).
        let reg = AdapterRegistry::with_builtins();
        let picked = reg.detect_best(&info()).await.expect("some");
        assert_eq!(picked.name(), "shell");
    }

    #[tokio::test]
    async fn builtins_fall_back_to_generic_for_unknown() {
        let reg = AdapterRegistry::with_builtins();
        let unknown_info = PaneInfo {
            argv: vec!["mystery-program".into()],
            comm: "mystery-program".into(),
            first_bytes: Vec::new(),
            env: std::collections::BTreeMap::new(),
        };
        let picked = reg.detect_best(&unknown_info).await.expect("some");
        assert_eq!(picked.name(), "generic");
    }
}
