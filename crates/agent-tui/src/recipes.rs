//! `agent-tui ask <provider>` recipes — TOML-driven argv + output
//! extraction per AI CLI. Bundled recipes ship via `include_str!`;
//! user-supplied drop-ins live in `~/.config/agent-tui/recipes/`
//! and override the bundled ones by name.
//!
//! Schema (TOML):
//!
//! ```toml
//! argv = ["claude", "-p"]             # required; child argv
//! default_max_ms = 60_000             # optional; per-call timeout
//! wrap_in_bash_cat = false            # optional; if true, becomes
//!                                     # `bash -c "cat | <argv...>"`
//!                                     # — needed for CLIs that
//!                                     # refuse a daemon-supplied
//!                                     # pipe-stdin (opencode).
//! extract_after_line = "codex"        # optional; output extractor
//! extract_until_line = "tokens used"  # optional; output extractor
//! ```
//!
//! Lookup order at startup:
//!
//!   1. `$AGENT_TUI_RECIPES_DIR` (explicit override)
//!   2. `$XDG_CONFIG_HOME/agent-tui/recipes/`
//!   3. `$HOME/.config/agent-tui/recipes/`
//!
//! On collision (user has `claude.toml`, bundled also has one), the
//! user version wins. Malformed recipes log a warning and are
//! skipped — the daemon doesn't crash on a bad drop-in.

use anyhow::{Result, anyhow};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Compiled-in starter recipes. Add a new (`name`, TOML content)
/// entry here to ship a new CLI by default. Users still override via
/// the drop-in directory.
const BUNDLED: &[(&str, &str)] = &[
    ("claude", include_str!("../recipes/claude.toml")),
    ("codex", include_str!("../recipes/codex.toml")),
    ("opencode", include_str!("../recipes/opencode.toml")),
    ("pi", include_str!("../recipes/pi.toml")),
];

/// One ask-recipe — argv + optional shaping for stdin/output.
#[derive(Debug, Clone, Deserialize)]
pub struct Recipe {
    /// The child's argv.
    pub argv: Vec<String>,
    /// Default `--max` (ms) when the caller doesn't override.
    #[serde(default)]
    pub default_max_ms: Option<u64>,
    /// If true, the recipe runs as `bash -c "cat | <argv...>"`.
    /// Use for CLIs that refuse a daemon-supplied pipe-stdin.
    #[serde(default)]
    pub wrap_in_bash_cat: bool,
    /// Extract lines AFTER the line matching this exact text.
    /// Combined with `extract_until_line`, produces a substring of
    /// the child's output without the surrounding metadata.
    #[serde(default)]
    pub extract_after_line: Option<String>,
    /// Stop extracting at the line matching this exact text.
    /// Required together with `extract_after_line`; using one without
    /// the other is allowed but has no effect.
    #[serde(default)]
    pub extract_until_line: Option<String>,
}

impl Recipe {
    /// Parse a TOML string. Validates that `argv` is non-empty.
    pub fn from_toml(s: &str) -> Result<Self> {
        let r: Self = toml::from_str(s).map_err(|e| anyhow!("recipe parse: {e}"))?;
        if r.argv.is_empty() {
            return Err(anyhow!("recipe `argv` must be non-empty"));
        }
        Ok(r)
    }

    /// Apply the extractor (if configured) to the child's full
    /// output. Returns the unmodified output when no extractor is
    /// set OR when the markers aren't found (best-effort).
    #[must_use]
    pub fn extract(&self, raw: &str) -> String {
        let (Some(after), Some(until)) = (&self.extract_after_line, &self.extract_until_line)
        else {
            return raw.to_string();
        };
        let mut in_block = false;
        let mut out = String::new();
        for line in raw.lines() {
            if !in_block {
                if line.trim() == after.trim() {
                    in_block = true;
                }
                continue;
            }
            if line.trim() == until.trim() {
                break;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(line);
        }
        // Fall back to raw if the markers weren't found at all.
        if out.is_empty() && !in_block {
            return raw.to_string();
        }
        out
    }

    /// argv with `wrap_in_bash_cat` applied (if set).
    #[must_use]
    pub fn effective_argv(&self) -> Vec<String> {
        if self.wrap_in_bash_cat {
            let pipeline = self.argv.join(" ");
            vec!["bash".into(), "-c".into(), format!("cat | {pipeline}")]
        } else {
            self.argv.clone()
        }
    }
}

/// Registry of named recipes. Built once at process start.
pub struct RecipeRegistry {
    entries: BTreeMap<String, Recipe>,
}

impl RecipeRegistry {
    /// Build with the bundled recipes plus any user drop-ins. User
    /// drop-ins override same-named bundled recipes.
    #[must_use]
    pub fn load() -> Self {
        let mut entries: BTreeMap<String, Recipe> = BTreeMap::new();
        // Bundled recipes — see `BUNDLED` const above.
        for (name, content) in BUNDLED {
            match Recipe::from_toml(content) {
                Ok(r) => {
                    entries.insert((*name).to_string(), r);
                }
                Err(e) => {
                    tracing::warn!(
                        recipe = %name,
                        error = %e,
                        "skipping malformed bundled recipe"
                    );
                }
            }
        }
        // 2. User drop-ins override bundled. Same dir-resolution
        //    rules as adapter manifests.
        if let Some(dir) = resolve_user_recipes_dir()
            && let Ok(read) = std::fs::read_dir(&dir)
        {
            for entry in read.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let content = match std::fs::read_to_string(&path) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "unreadable recipe");
                        continue;
                    }
                };
                match Recipe::from_toml(&content) {
                    Ok(r) => {
                        tracing::debug!(
                            recipe = %stem,
                            path = %path.display(),
                            "loaded user recipe"
                        );
                        entries.insert(stem.to_string(), r);
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "skipping malformed user recipe"
                        );
                    }
                }
            }
        }
        Self { entries }
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Recipe> {
        self.entries.get(name)
    }

    #[must_use]
    pub fn known(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }
}

fn resolve_user_recipes_dir() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("AGENT_TUI_RECIPES_DIR") {
        return Some(std::path::PathBuf::from(p));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let mut p = std::path::PathBuf::from(xdg);
        p.push("agent-tui/recipes");
        return Some(p);
    }
    let home = std::env::var_os("HOME")?;
    let mut p = std::path::PathBuf::from(home);
    p.push(".config/agent-tui/recipes");
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal() {
        let r = Recipe::from_toml(r#"argv = ["x", "-y"]"#).unwrap();
        assert_eq!(r.argv, vec!["x", "-y"]);
        assert!(!r.wrap_in_bash_cat);
    }

    #[test]
    fn rejects_empty_argv() {
        assert!(Recipe::from_toml("argv = []").is_err());
    }

    #[test]
    fn extract_between_markers() {
        let r = Recipe::from_toml(
            r#"
                argv = ["x"]
                extract_after_line = "codex"
                extract_until_line = "tokens used"
            "#,
        )
        .unwrap();
        let raw = "user\nprompt\ncodex\n42\ntokens used\n9999\n";
        assert_eq!(r.extract(raw), "42");
    }

    #[test]
    fn extract_falls_back_to_raw_on_missing_marker() {
        let r = Recipe::from_toml(
            r#"
                argv = ["x"]
                extract_after_line = "MISSING"
                extract_until_line = "ALSO_MISSING"
            "#,
        )
        .unwrap();
        let raw = "no markers here\nsecond line\n";
        assert_eq!(r.extract(raw), raw);
    }

    #[test]
    fn effective_argv_wraps_in_bash() {
        let r = Recipe::from_toml(
            r#"
                argv = ["opencode", "run"]
                wrap_in_bash_cat = true
            "#,
        )
        .unwrap();
        assert_eq!(r.effective_argv(), vec!["bash", "-c", "cat | opencode run"]);
    }

    #[test]
    fn registry_loads_bundled() {
        let reg = RecipeRegistry::load();
        assert!(reg.get("claude").is_some());
        assert!(reg.get("codex").is_some());
        assert!(reg.get("opencode").is_some());
        assert!(reg.get("pi").is_some());
    }

    #[test]
    fn ai_sdk_harness_cli_recipes_pin_expected_binaries() {
        let reg = RecipeRegistry::load();
        assert_eq!(
            reg.get("claude").expect("claude recipe").argv,
            vec!["claude", "-p"]
        );
        assert_eq!(
            reg.get("codex").expect("codex recipe").argv,
            vec!["codex", "exec"]
        );
        assert_eq!(
            reg.get("pi").expect("pi recipe").argv,
            vec!["pi", "--print"]
        );
    }
}
