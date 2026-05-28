//! Manifest-driven adapters.
//!
//! A small TOML schema that describes a TUI app's layout — panels,
//! anchors, banners — so adding adapter coverage for a new app
//! doesn't require Rust code. Built for the long tail of TUI apps
//! (helix, k9s, btop, ranger, gitui, …) where writing a bespoke
//! `Adapter` impl per app doesn't scale.
//!
//! Schema (TOML):
//!
//! ```toml
//! name = "lazygit"                # adapter id (returned by Adapter::name)
//!
//! [detect]
//! argv0 = ["lazygit"]             # exact basename matches → confidence 0.9
//! banner_regex = '^lazygit '      # optional: first-bytes regex
//!
//! [[regions]]
//! name = "status"                 # outline-node display name (informational)
//! role = "status-bar"             # outline-node role
//! rows = [0, 2]                   # row range (inclusive). Negative = from-end.
//! cols = [0, -1]                  # optional; defaults to full width
//! ```
//!
//! Row/col ranges are inclusive. Negative indices count from the
//! end of the grid (`-1` = last row). Empty regions are dropped from
//! the outline so a screen with only some regions filled doesn't
//! emit blank nodes.
//!
//! ## Drop-in directory
//!
//! At daemon startup, manifests in the user's adapter dir are loaded
//! and override any bundled manifest with the same `name`. Lookup
//! order:
//!
//!   1. `$AGENT_TUI_ADAPTERS_DIR` (explicit override)
//!   2. `$XDG_CONFIG_HOME/agent-tui/adapters/`
//!   3. `$HOME/.config/agent-tui/adapters/`
//!
//! Non-`.toml` files are skipped; malformed manifests log a warning
//! and don't crash the daemon. To add a new adapter, drop a TOML file
//! in the dir and respawn the daemon — no rebuild required.

use crate::{Adapter, AdapterError, Capabilities, EngineSnapshot, Outline, PaneInfo};
use agent_tui_protocol::OutlineNode;
use serde::{Deserialize, Serialize};

/// One adapter manifest loaded from TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterManifest {
    /// Adapter id (returned by `Adapter::name`). Conventionally
    /// kebab-case, matches the manifest filename stem.
    pub name: String,
    /// Detection criteria — argv-basename + optional banner regex.
    #[serde(default)]
    pub detect: DetectSpec,
    /// Outline regions in display order.
    #[serde(default)]
    pub regions: Vec<RegionSpec>,
}

/// Detection criteria. Empty = adapter never matches (manual-only).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DetectSpec {
    /// Exact basename matches against `PaneInfo::comm`.
    #[serde(default)]
    pub argv0: Vec<String>,
    /// Substring matches against any element of `PaneInfo::argv`.
    #[serde(default)]
    pub argv_contains: Vec<String>,
    /// Regex to match against the first ~512 bytes of PTY output.
    /// Compiled at load time; bad regexes fail the manifest.
    #[serde(default)]
    pub banner_regex: Option<String>,
}

/// One region of the grid mapped to one outline node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionSpec {
    /// Outline-node display name. Informational only; the node's
    /// rendered text comes from the grid cells inside the region.
    pub name: String,
    /// Outline-node role (e.g. `"status-bar"`, `"list"`, `"footer"`).
    pub role: String,
    /// Inclusive row range. Negative indices count from end.
    pub rows: [i32; 2],
    /// Inclusive col range. Defaults to `[0, -1]` (full width).
    #[serde(default = "default_cols")]
    pub cols: [i32; 2],
}

fn default_cols() -> [i32; 2] {
    [0, -1]
}

impl AdapterManifest {
    /// Parse from a TOML string.
    pub fn from_toml(s: &str) -> Result<Self, ManifestError> {
        let mut m: Self = toml::from_str(s).map_err(|e| ManifestError::Parse(e.to_string()))?;
        // Validate the banner regex up front so detect() never errors.
        if let Some(re) = &m.detect.banner_regex {
            regex::Regex::new(re).map_err(|e| ManifestError::BadRegex(e.to_string()))?;
        }
        // Normalize: empty name is meaningless.
        if m.name.trim().is_empty() {
            return Err(ManifestError::EmptyName);
        }
        m.name = m.name.trim().to_string();
        Ok(m)
    }
}

/// Errors from loading a manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// TOML parse failure.
    #[error("manifest parse: {0}")]
    Parse(String),
    /// `detect.banner_regex` failed to compile.
    #[error("bad banner regex: {0}")]
    BadRegex(String),
    /// `name` field was empty.
    #[error("manifest must have a non-empty `name`")]
    EmptyName,
}

/// Adapter that interprets an [`AdapterManifest`] at runtime.
///
/// Holds a leaked `&'static str` for the adapter name so the
/// `Adapter::name() -> &'static str` contract is satisfied. The leak
/// is per-manifest (a handful of bytes per adapter) and matches the
/// total-leaks model used by clap and other "load-once" libraries.
pub struct ManifestAdapter {
    name_static: &'static str,
    manifest: AdapterManifest,
}

impl ManifestAdapter {
    /// Build from a parsed manifest. The adapter's `name()` becomes
    /// `manifest.name` (leaked to `'static`).
    #[must_use]
    pub fn new(manifest: AdapterManifest) -> Self {
        let name_static: &'static str = Box::leak(manifest.name.clone().into_boxed_str());
        Self {
            name_static,
            manifest,
        }
    }
}

#[async_trait::async_trait]
impl Adapter for ManifestAdapter {
    fn name(&self) -> &'static str {
        self.name_static
    }

    async fn detect(&self, info: &PaneInfo) -> f32 {
        // Strongest signal: argv-basename exact match.
        if self.manifest.detect.argv0.iter().any(|s| s == &info.comm) {
            return 0.9;
        }
        // Next: argv substring (handles wrapper scripts like
        // `bash -c "lazygit ..."` — argv[0] = bash, argv contains lazygit).
        if self
            .manifest
            .detect
            .argv_contains
            .iter()
            .any(|needle| info.argv.iter().any(|a| a.contains(needle)))
        {
            return 0.7;
        }
        // Banner regex (first-bytes redetect). We don't have access
        // to first-bytes from `PaneInfo` directly in this prototype;
        // the daemon's re-detect pass calls detect() again later with
        // first_bytes populated — at that point the regex matches.
        if let Some(re) = &self.manifest.detect.banner_regex
            && let Ok(re) = regex::Regex::new(re)
            && let Ok(s) = std::str::from_utf8(&info.first_bytes)
            && re.is_match(s)
        {
            return 0.85;
        }
        0.0
    }

    async fn initialize(&self) -> Result<Capabilities, AdapterError> {
        Ok(Capabilities::default())
    }

    async fn outline(&self, snap: &EngineSnapshot) -> Result<Outline, AdapterError> {
        let rows = usize::from(snap.grid.rows);
        let cols = usize::from(snap.grid.cols);
        let mut nodes: Vec<OutlineNode> = Vec::with_capacity(self.manifest.regions.len());
        let mut next_idx: u32 = 1;
        for region in &self.manifest.regions {
            let (r0, r1) = resolve_range(region.rows, rows);
            let (c0, c1) = resolve_range(region.cols, cols);
            if r0 > r1 || c0 > c1 {
                continue;
            }
            let text = render_region(snap, r0, r1, c0, c1, cols);
            if text.trim().is_empty() {
                continue;
            }
            nodes.push(OutlineNode {
                r#ref: format!("@e{next_idx}"),
                role: region.role.clone(),
                name: text,
                value: None,
                focused: false,
                anchor: Some((
                    u16::try_from(r0).unwrap_or(0),
                    u16::try_from(c0).unwrap_or(0),
                )),
                children: Vec::new(),
                ..OutlineNode::default()
            });
            next_idx += 1;
        }
        Ok(Outline {
            adapter: self.manifest.name.clone(),
            nodes,
        })
    }

    async fn eval(&self, _expr: &str) -> Result<serde_json::Value, AdapterError> {
        Err(AdapterError::Refused(
            "manifest adapters don't support eval".into(),
        ))
    }

    async fn shutdown(&self) -> Result<(), AdapterError> {
        Ok(())
    }
}

/// Resolve `[start, end]` against a grid dimension. Negative indices
/// count from the end (`-1` = last). Clamps to `[0, len-1]`.
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]
fn resolve_range(spec: [i32; 2], len: usize) -> (usize, usize) {
    if len == 0 {
        return (0, 0);
    }
    // Manifest ranges are bounded by terminal dimensions (typically
    // 80×24, never anywhere near i32::MAX); the cast width here is
    // safe in practice. Using i32 lets negative indices count from
    // the end (`-1` = last row).
    let max = (len - 1) as i32;
    let norm = |v: i32| -> usize {
        let v = if v < 0 { (len as i32) + v } else { v };
        v.clamp(0, max) as usize
    };
    let s = norm(spec[0]);
    let e = norm(spec[1]);
    if s <= e { (s, e) } else { (e, s) }
}

/// Render rows [r0..=r1] × cols [c0..=c1] as a multi-line string.
/// Trims trailing whitespace per row and drops trailing empty rows.
fn render_region(
    snap: &EngineSnapshot,
    r0: usize,
    r1: usize,
    c0: usize,
    c1: usize,
    grid_cols: usize,
) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(r1 - r0 + 1);
    for row in r0..=r1 {
        let mut line = String::new();
        let base = row * grid_cols;
        for col in c0..=c1 {
            if base + col >= snap.grid.cells.len() {
                break;
            }
            let cell = &snap.grid.cells[base + col];
            if cell.ch.is_empty() {
                line.push(' ');
            } else {
                line.push_str(&cell.ch);
            }
        }
        lines.push(line.trim_end().to_string());
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_manifest() {
        let toml = r#"
            name = "demo"
            [detect]
            argv0 = ["demo"]
            [[regions]]
            name = "top"
            role = "status-bar"
            rows = [0, 1]
        "#;
        let m = AdapterManifest::from_toml(toml).unwrap();
        assert_eq!(m.name, "demo");
        assert_eq!(m.detect.argv0, vec!["demo".to_string()]);
        assert_eq!(m.regions.len(), 1);
        assert_eq!(m.regions[0].rows, [0, 1]);
        assert_eq!(m.regions[0].cols, [0, -1]);
    }

    #[test]
    fn rejects_empty_name() {
        let toml = r#"name = """#;
        assert!(AdapterManifest::from_toml(toml).is_err());
    }

    #[test]
    fn rejects_bad_regex() {
        let toml = r#"
            name = "x"
            [detect]
            banner_regex = "[unclosed"
        "#;
        assert!(AdapterManifest::from_toml(toml).is_err());
    }

    #[test]
    fn resolve_range_negative_indices() {
        // grid of 24 rows, range [-3, -1] → (21, 23)
        assert_eq!(resolve_range([-3, -1], 24), (21, 23));
        // (-1, 0) flipped to (0, 23)
        assert_eq!(resolve_range([-1, 0], 24), (0, 23));
        // clamped: [-100, 100] → (0, 23)
        assert_eq!(resolve_range([-100, 100], 24), (0, 23));
    }
}
