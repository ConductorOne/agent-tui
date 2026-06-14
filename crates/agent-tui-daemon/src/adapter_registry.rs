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

use agent_tui_adapter::{
    Adapter, ClaudeCodeAdapter, GenericAdapter, PaneInfo, ShellAdapter, VimAdapter,
};

const MIN_CONFIDENCE: f32 = 0.05;

/// Read-only collection of attachable adapters.
#[derive(Clone, Default)]
pub struct AdapterRegistry {
    adapters: Vec<Arc<dyn Adapter>>,
}

impl AdapterRegistry {
    /// Build a registry pre-populated with the v1 built-ins:
    /// `generic` (fallback), `claude-code`, `shell`, `vim`.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut reg = Self {
            adapters: vec![
                Arc::new(GenericAdapter),
                Arc::new(ClaudeCodeAdapter),
                Arc::new(ShellAdapter),
                Arc::new(VimAdapter),
            ],
        };
        // Order matters for the "user overrides bundled" rule:
        // load user manifests FIRST (to learn their names), then
        // load bundled manifests SKIPPING any name a user manifest
        // already claimed.
        let user_names = reg.load_user_adapters();
        reg.load_bundled_manifests(&user_names);
        reg
    }

    /// Load adapter manifests from the user's config directory.
    /// Honors `$AGENT_TUI_ADAPTERS_DIR` first, then
    /// `$XDG_CONFIG_HOME/agent-tui/adapters/`, then
    /// `$HOME/.config/agent-tui/adapters/`. Scans `*.toml` files
    /// in the chosen directory (non-recursive).
    ///
    /// Returns the set of manifest `name` fields the user provided,
    /// so the bundled-loader can skip same-named built-ins (user
    /// wins on collision).
    ///
    /// Bad files emit a tracing warning and are skipped; a malformed
    /// drop-in never crashes the daemon.
    fn load_user_adapters(&mut self) -> std::collections::BTreeSet<String> {
        let mut user_names = std::collections::BTreeSet::new();
        let Some(dir) = resolve_user_adapter_dir() else {
            return user_names;
        };
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                // ENOENT is the common case (user has no adapters
                // dir at all) — silent. Other errors get a warning.
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        dir = %dir.display(),
                        error = %e,
                        "could not read user adapter directory"
                    );
                }
                return user_names;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "skipping unreadable adapter manifest"
                    );
                    continue;
                }
            };
            match agent_tui_adapter::AdapterManifest::from_toml(&content) {
                Ok(m) => {
                    let name = m.name.clone();
                    tracing::info!(
                        adapter = %name,
                        path = %path.display(),
                        "loaded user adapter manifest"
                    );
                    user_names.insert(name);
                    self.adapters
                        .push(Arc::new(agent_tui_adapter::ManifestAdapter::new(m)));
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "skipping malformed adapter manifest"
                    );
                }
            }
        }
        user_names
    }

    /// Load adapter manifests that are bundled into the binary via
    /// `include_str!`. Skips any manifest whose name a user-provided
    /// drop-in already registered. Bad bundled manifests log a
    /// warning and don't crash the daemon.
    fn load_bundled_manifests(&mut self, skip_names: &std::collections::BTreeSet<String>) {
        // Add manifests here as they land. Each is a single
        // `include_str!` of a `.toml` file in `agent-tui-adapter/
        // manifests/`. Order doesn't matter — detect-confidence
        // ranks them at use time.
        const BUNDLED: &[(&str, &str)] = &[
            (
                "claude-code",
                include_str!("../../agent-tui-adapter/manifests/claude-code.toml"),
            ),
            (
                "codex",
                include_str!("../../agent-tui-adapter/manifests/codex.toml"),
            ),
            (
                "pi",
                include_str!("../../agent-tui-adapter/manifests/pi.toml"),
            ),
            (
                "lazygit",
                include_str!("../../agent-tui-adapter/manifests/lazygit.toml"),
            ),
            (
                "tig",
                include_str!("../../agent-tui-adapter/manifests/tig.toml"),
            ),
            (
                "htop",
                include_str!("../../agent-tui-adapter/manifests/htop.toml"),
            ),
            (
                "less",
                include_str!("../../agent-tui-adapter/manifests/less.toml"),
            ),
            (
                "fzf",
                include_str!("../../agent-tui-adapter/manifests/fzf.toml"),
            ),
            (
                "top",
                include_str!("../../agent-tui-adapter/manifests/top.toml"),
            ),
        ];
        for (name, content) in BUNDLED {
            if skip_names.contains(*name) {
                tracing::info!(
                    adapter = %name,
                    "user manifest overrides bundled"
                );
                continue;
            }
            match agent_tui_adapter::AdapterManifest::from_toml(content) {
                Ok(m) => {
                    self.adapters
                        .push(Arc::new(agent_tui_adapter::ManifestAdapter::new(m)));
                }
                Err(e) => {
                    tracing::warn!(
                        adapter = %name,
                        error = %e,
                        "skipping malformed bundled adapter manifest"
                    );
                }
            }
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

/// Resolve the user's adapter-manifest directory in order of preference:
///   1. `$AGENT_TUI_ADAPTERS_DIR` — explicit override (tests, CI).
///   2. `$XDG_CONFIG_HOME/agent-tui/adapters/` — XDG-compliant.
///   3. `$HOME/.config/agent-tui/adapters/` — fallback when
///      `XDG_CONFIG_HOME` isn't set (most platforms default this).
///
/// Returns `None` only if HOME is also unset (very unusual).
fn resolve_user_adapter_dir() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("AGENT_TUI_ADAPTERS_DIR") {
        return Some(std::path::PathBuf::from(p));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let mut p = std::path::PathBuf::from(xdg);
        p.push("agent-tui/adapters");
        return Some(p);
    }
    let home = std::env::var_os("HOME")?;
    let mut p = std::path::PathBuf::from(home);
    p.push(".config/agent-tui/adapters");
    Some(p)
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

    #[tokio::test]
    async fn builtins_pick_provider_manifests_for_ai_agents() {
        let reg = AdapterRegistry::with_builtins();
        for (comm, adapter) in [
            ("claude", "claude-code"),
            ("claude-code", "claude-code"),
            ("codex", "codex"),
            ("pi", "pi"),
        ] {
            let picked = reg
                .detect_best(&PaneInfo {
                    argv: vec![format!("/usr/bin/{comm}")],
                    comm: comm.into(),
                    first_bytes: Vec::new(),
                    env: std::collections::BTreeMap::new(),
                })
                .await
                .expect("some");
            assert_eq!(picked.name(), adapter, "{comm}");
        }
    }
}
