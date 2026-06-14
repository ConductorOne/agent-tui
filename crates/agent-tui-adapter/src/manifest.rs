//! Manifest-driven adapters.
//!
//! A small TOML schema that describes a TUI app's layout — panels,
//! anchors, banners — so adding adapter coverage for a new app
//! doesn't require Rust code. Built for the long tail of TUI apps
//! (helix, k9s, btop, ranger, gitui, …) where writing a bespoke
//! `Adapter` impl per app doesn't scale.
//!
//! Schema v2 (TOML):
//!
//! ```toml
//! version = 2
//! name = "lazygit"                # adapter id (returned by Adapter::name)
//! root = "lazygit"                # root ref (`@lazygit`)
//!
//! [detect]
//! argv0 = ["lazygit"]             # exact basename matches → confidence 0.95
//! argv_excludes = ["--help"]      # any match here rejects this manifest
//! banner_regex = '^lazygit '      # optional: first-bytes regex
//!
//! [[regions]]
//! id = "status"                   # child ref (`@lazygit.status`)
//! role = "status-bar"             # outline-node role
//! rows = [0, 2]                   # range or "last-non-empty"/"above-last-non-empty"
//! cols = [0, -1]                  # optional; defaults to full width
//! focus = "cursor"                # optional: never|always|cursor
//!
//! [[signals]]
//! id = "done"                     # child ref (`@lazygit.done`)
//! role = "status"
//! source = "status"               # optional region id; omitted = whole screen
//! regex = '(?i)\bdone\b'
//! name = "done"                   # optional; defaults to captured value/match
//! ```
//!
//! Row/col ranges are inclusive. Negative indices count from the
//! end of the grid (`-1` = last row). Empty regions are dropped from
//! the outline so a screen with only some regions filled doesn't emit
//! blank nodes. Region and signal refs are adapter-durable.
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
use std::collections::{BTreeMap, BTreeSet};

/// One adapter manifest loaded from TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterManifest {
    /// Manifest schema version. v2 is intentionally not backward-compatible
    /// with the original positional `@eN` region schema.
    pub version: u8,
    /// Adapter id (returned by `Adapter::name`). Conventionally
    /// kebab-case, matches the manifest filename stem.
    pub name: String,
    /// Durable root ref head. A manifest with `root = "top"` emits `@top`
    /// with children such as `@top.processes`.
    pub root: String,
    /// Detection criteria — argv-basename + optional banner regex.
    #[serde(default)]
    pub detect: DetectSpec,
    /// Outline regions in display order.
    #[serde(default)]
    pub regions: Vec<RegionSpec>,
    /// Regex-derived state nodes in display order.
    #[serde(default)]
    pub signals: Vec<SignalSpec>,
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
    /// Substring matches against any element of `PaneInfo::argv` that reject
    /// this manifest before positive signals are scored. Use this for
    /// non-interactive subcommands/flags such as `--print`, `exec`, `--help`.
    #[serde(default)]
    pub argv_excludes: Vec<String>,
    /// Regex to match against the first ~512 bytes of PTY output.
    /// Compiled at load time; bad regexes fail the manifest.
    #[serde(default)]
    pub banner_regex: Option<String>,
}

/// One region of the grid mapped to one outline node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionSpec {
    /// Durable child ref segment below `root`.
    pub id: String,
    /// Outline-node role (e.g. `"status-bar"`, `"list"`, `"footer"`).
    pub role: String,
    /// Row range or dynamic row selector.
    pub rows: RowSpec,
    /// Inclusive col range. Defaults to `[0, -1]` (full width).
    #[serde(default = "default_cols")]
    pub cols: [i32; 2],
    /// How this region reports focus.
    #[serde(default)]
    pub focus: RegionFocus,
}

/// Rows selected by a manifest region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RowSpec {
    /// Inclusive row range. Negative indices count from end.
    Range([i32; 2]),
    /// Dynamic row selector.
    Dynamic(RowDynamic),
}

/// Dynamic row selectors for prompt-oriented CLIs whose active row depends on
/// scrollback height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RowDynamic {
    /// The last non-empty visible row.
    LastNonEmpty,
    /// From row 0 through the row above the last non-empty visible row.
    AboveLastNonEmpty,
    /// The terminal cursor row.
    Cursor,
    /// From row 0 through the row above the terminal cursor.
    AboveCursor,
}

/// Focus policy for a manifest region.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegionFocus {
    /// Region never reports focused.
    #[default]
    Never,
    /// Region always reports focused when present.
    Always,
    /// Region reports focused when the terminal cursor is inside its rect.
    Cursor,
}

/// Regex-derived state node emitted when `regex` matches a source region or
/// the whole visible screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalSpec {
    /// Durable child ref segment below `root`.
    pub id: String,
    /// Outline-node role (for example `"approval-request"` or
    /// `"file-change"`).
    pub role: String,
    /// Optional source region id. When omitted, the signal scans the whole
    /// visible screen.
    #[serde(default)]
    pub source: Option<String>,
    /// Regex evaluated against the selected source. If it has a capture group,
    /// capture 1 becomes the node value; otherwise the full match does.
    pub regex: String,
    /// Optional stable display name. When omitted, the node name is the
    /// captured value/full match.
    #[serde(default)]
    pub name: Option<String>,
}

fn default_cols() -> [i32; 2] {
    [0, -1]
}

fn validate_unique_segments(m: &AdapterManifest) -> Result<(), ManifestError> {
    let mut children = BTreeSet::new();
    let regions: BTreeSet<String> = m.regions.iter().map(|r| r.id.clone()).collect();
    for region in &m.regions {
        if !is_ref_segment(&region.id) {
            return Err(ManifestError::BadRefSegment(region.id.clone()));
        }
        if !children.insert(region.id.clone()) {
            return Err(ManifestError::DuplicateRef(region.id.clone()));
        }
    }
    for signal in &m.signals {
        if !is_ref_segment(&signal.id) {
            return Err(ManifestError::BadRefSegment(signal.id.clone()));
        }
        if !children.insert(signal.id.clone()) {
            return Err(ManifestError::DuplicateRef(signal.id.clone()));
        }
        if let Some(source) = &signal.source
            && !regions.contains(source)
        {
            return Err(ManifestError::UnknownSignalSource(source.clone()));
        }
    }
    Ok(())
}

fn is_ref_segment(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn region_focused(
    region: &RegionSpec,
    snap: &EngineSnapshot,
    r0: usize,
    r1: usize,
    c0: usize,
    c1: usize,
) -> bool {
    match region.focus {
        RegionFocus::Never => false,
        RegionFocus::Always => true,
        RegionFocus::Cursor => {
            let (row, col) = snap.grid.cursor;
            let row = usize::from(row);
            let col = usize::from(col);
            row >= r0 && row <= r1 && col >= c0 && col <= c1
        }
    }
}

impl AdapterManifest {
    /// Parse from a TOML string.
    pub fn from_toml(s: &str) -> Result<Self, ManifestError> {
        let mut m: Self = toml::from_str(s).map_err(|e| ManifestError::Parse(e.to_string()))?;
        if m.version != 2 {
            return Err(ManifestError::BadVersion(m.version));
        }
        // Validate the banner regex up front so detect() never errors.
        if let Some(re) = &m.detect.banner_regex {
            regex::Regex::new(re).map_err(|e| ManifestError::BadRegex(e.to_string()))?;
        }
        for signal in &m.signals {
            regex::Regex::new(&signal.regex).map_err(|e| ManifestError::BadRegex(e.to_string()))?;
        }
        // Normalize: empty name is meaningless.
        if m.name.trim().is_empty() {
            return Err(ManifestError::EmptyName);
        }
        m.name = m.name.trim().to_string();
        m.root = m.root.trim().to_string();
        if !is_ref_segment(&m.root) {
            return Err(ManifestError::BadRefSegment(m.root));
        }
        validate_unique_segments(&m)?;
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
    /// Only v2 manifests are accepted.
    #[error("unsupported manifest version {0}; expected version = 2")]
    BadVersion(u8),
    /// `name` field was empty.
    #[error("manifest must have a non-empty `name`")]
    EmptyName,
    /// `root`, region ids, and signal ids must be selector-safe.
    #[error("manifest ref segment must be ASCII alphanumeric/underscore/hyphen: {0:?}")]
    BadRefSegment(String),
    /// Region/signal ids share one child namespace.
    #[error("manifest has duplicate child ref segment: {0}")]
    DuplicateRef(String),
    /// Signal source named no region.
    #[error("manifest signal source {0:?} does not match a region id")]
    UnknownSignalSource(String),
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
        if self
            .manifest
            .detect
            .argv_excludes
            .iter()
            .any(|needle| info.argv.iter().any(|a| a.contains(needle)))
        {
            return 0.0;
        }

        let mut confidence: f32 = 0.0;
        // Strongest signal: argv-basename exact match.
        if self.manifest.detect.argv0.iter().any(|s| s == &info.comm) {
            confidence = confidence.max(0.95);
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
            confidence = confidence.max(0.7);
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
            confidence = confidence.max(0.85);
        }
        confidence
    }

    async fn initialize(&self) -> Result<Capabilities, AdapterError> {
        Ok(Capabilities::default())
    }

    async fn outline(&self, snap: &EngineSnapshot) -> Result<Outline, AdapterError> {
        let rows = usize::from(snap.grid.rows);
        let cols = usize::from(snap.grid.cols);
        let mut children: Vec<OutlineNode> =
            Vec::with_capacity(self.manifest.regions.len() + self.manifest.signals.len());
        let mut region_texts: BTreeMap<String, String> = BTreeMap::new();
        let screen_text = render_region(
            snap,
            0,
            rows.saturating_sub(1),
            0,
            cols.saturating_sub(1),
            cols,
        );
        for region in &self.manifest.regions {
            let Some((r0, r1)) = resolve_rows(&region.rows, snap, rows, cols) else {
                continue;
            };
            let (c0, c1) = resolve_range(region.cols, cols);
            if r0 > r1 || c0 > c1 {
                continue;
            }
            let text = render_region(snap, r0, r1, c0, c1, cols);
            region_texts.insert(region.id.clone(), text.clone());
            if text.trim().is_empty() {
                continue;
            }
            children.push(OutlineNode {
                r#ref: format!("@{}.{}", self.manifest.root, region.id),
                role: region.role.clone(),
                name: text,
                value: None,
                focused: region_focused(region, snap, r0, r1, c0, c1),
                anchor: Some((
                    u16::try_from(r0).unwrap_or(0),
                    u16::try_from(c0).unwrap_or(0),
                )),
                extent: Some((
                    u16::try_from(c1 - c0 + 1).unwrap_or(0),
                    u16::try_from(r1 - r0 + 1).unwrap_or(0),
                )),
                durable: true,
                children: Vec::new(),
                ..OutlineNode::default()
            });
        }
        for signal in &self.manifest.signals {
            let haystack = signal
                .source
                .as_ref()
                .and_then(|source| region_texts.get(source))
                .unwrap_or(&screen_text);
            let re = regex::Regex::new(&signal.regex).map_err(|e| {
                AdapterError::Refused(format!("manifest signal regex was not prevalidated: {e}"))
            })?;
            let Some(caps) = re.captures(haystack) else {
                continue;
            };
            let value = caps
                .get(1)
                .or_else(|| caps.get(0))
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            let name = signal.name.clone().unwrap_or_else(|| value.clone());
            children.push(OutlineNode {
                r#ref: format!("@{}.{}", self.manifest.root, signal.id),
                role: signal.role.clone(),
                name,
                value: Some(value),
                focused: false,
                anchor: None,
                extent: None,
                durable: true,
                children: Vec::new(),
                ..OutlineNode::default()
            });
        }
        let nodes = vec![OutlineNode {
            r#ref: format!("@{}", self.manifest.root),
            role: "root".into(),
            durable: true,
            children,
            ..OutlineNode::default()
        }];
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

/// Resolve a static or dynamic row selector against the current grid.
fn resolve_rows(
    spec: &RowSpec,
    snap: &EngineSnapshot,
    rows: usize,
    cols: usize,
) -> Option<(usize, usize)> {
    if rows == 0 {
        return None;
    }
    match spec {
        RowSpec::Range(range) => Some(resolve_range(*range, rows)),
        RowSpec::Dynamic(RowDynamic::LastNonEmpty) => {
            last_non_empty_row(snap, rows, cols).map(|row| (row, row))
        }
        RowSpec::Dynamic(RowDynamic::AboveLastNonEmpty) => {
            let row = last_non_empty_row(snap, rows, cols)?;
            (row > 0).then_some((0, row - 1))
        }
        RowSpec::Dynamic(RowDynamic::Cursor) => {
            let row = usize::from(snap.grid.cursor.0).min(rows - 1);
            Some((row, row))
        }
        RowSpec::Dynamic(RowDynamic::AboveCursor) => {
            let row = usize::from(snap.grid.cursor.0).min(rows - 1);
            (row > 0).then_some((0, row - 1))
        }
    }
}

fn last_non_empty_row(snap: &EngineSnapshot, rows: usize, cols: usize) -> Option<usize> {
    (0..rows)
        .rev()
        .find(|row| row_has_non_blank_cells(snap, *row, cols))
}

fn row_has_non_blank_cells(snap: &EngineSnapshot, row: usize, cols: usize) -> bool {
    let base = row * cols;
    (0..cols).any(|col| {
        snap.grid
            .cells
            .get(base + col)
            .is_some_and(|cell| !cell.ch.trim().is_empty())
    })
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
    use agent_tui_engine::{Cell, CellGrid, ModeFlags};

    #[test]
    fn parse_minimal_manifest() {
        let toml = r#"
            version = 2
            name = "demo"
            root = "demo"
            [detect]
            argv0 = ["demo"]
            [[regions]]
            id = "top"
            role = "status-bar"
            rows = [0, 1]
        "#;
        let m = AdapterManifest::from_toml(toml).unwrap();
        assert_eq!(m.version, 2);
        assert_eq!(m.name, "demo");
        assert_eq!(m.root, "demo");
        assert_eq!(m.detect.argv0, vec!["demo".to_string()]);
        assert!(m.detect.argv_excludes.is_empty());
        assert_eq!(m.regions.len(), 1);
        assert_eq!(m.regions[0].rows, RowSpec::Range([0, 1]));
        assert_eq!(m.regions[0].cols, [0, -1]);
    }

    #[test]
    fn rejects_empty_name() {
        let toml = r#"
            version = 2
            name = ""
            root = "demo"
        "#;
        assert!(AdapterManifest::from_toml(toml).is_err());
    }

    #[test]
    fn rejects_bad_regex() {
        let toml = r#"
            version = 2
            name = "x"
            root = "x"
            [detect]
            banner_regex = "[unclosed"
        "#;
        assert!(AdapterManifest::from_toml(toml).is_err());
    }

    #[test]
    fn rejects_v1_manifest_without_version() {
        let toml = r#"
            name = "demo"
            [[regions]]
            name = "body"
            role = "buffer"
            rows = [0, -1]
        "#;
        assert!(matches!(
            AdapterManifest::from_toml(toml),
            Err(ManifestError::Parse(_))
        ));
    }

    #[test]
    fn rejects_duplicate_child_refs() {
        let toml = r#"
            version = 2
            name = "demo"
            root = "demo"
            [[regions]]
            id = "state"
            role = "buffer"
            rows = [0, -1]
            [[signals]]
            id = "state"
            role = "status"
            regex = "done"
        "#;
        assert!(matches!(
            AdapterManifest::from_toml(toml),
            Err(ManifestError::DuplicateRef(_))
        ));
    }

    #[test]
    fn rejects_unknown_signal_source() {
        let toml = r#"
            version = 2
            name = "demo"
            root = "demo"
            [[signals]]
            id = "done"
            role = "status"
            source = "missing"
            regex = "done"
        "#;
        assert!(matches!(
            AdapterManifest::from_toml(toml),
            Err(ManifestError::UnknownSignalSource(_))
        ));
    }

    #[tokio::test]
    async fn v2_outline_emits_durable_region_and_signal_refs() {
        let manifest = AdapterManifest::from_toml(
            r#"
            version = 2
            name = "demo"
            root = "demo"

            [[regions]]
            id = "response"
            role = "response"
            rows = "above-last-non-empty"

            [[regions]]
            id = "input"
            role = "input"
            rows = "last-non-empty"
            focus = "cursor"

            [[signals]]
            id = "approval"
            role = "approval-request"
            source = "response"
            regex = "approval: (.+)"
            name = "approval requested"

            [[signals]]
            id = "file-change"
            role = "file-change"
            source = "response"
            regex = "status: file-change (.+)"
            "#,
        )
        .unwrap();
        let outline = ManifestAdapter::new(manifest)
            .outline(&snap(
                "plan\napproval: type approve\nstatus: file-change release-note.md\n> ",
            ))
            .await
            .unwrap();
        assert_eq!(outline.adapter, "demo");
        assert_eq!(outline.nodes.len(), 1);
        let root = &outline.nodes[0];
        assert_eq!(root.r#ref, "@demo");
        assert!(root.durable);

        let response = root
            .children
            .iter()
            .find(|n| n.r#ref == "@demo.response")
            .expect("response ref");
        assert_eq!(response.role, "response");
        assert!(response.durable);

        let input = root
            .children
            .iter()
            .find(|n| n.r#ref == "@demo.input")
            .expect("input ref");
        assert!(input.focused);

        let approval = root
            .children
            .iter()
            .find(|n| n.r#ref == "@demo.approval")
            .expect("approval signal");
        assert_eq!(approval.role, "approval-request");
        assert_eq!(approval.name, "approval requested");
        assert_eq!(approval.value.as_deref(), Some("type approve"));

        let file_change = root
            .children
            .iter()
            .find(|n| n.r#ref == "@demo.file-change")
            .expect("file-change signal");
        assert_eq!(file_change.value.as_deref(), Some("release-note.md"));
    }

    #[tokio::test]
    async fn detect_rejects_excluded_argv_shape() {
        let manifest = AdapterManifest::from_toml(
            r#"
            version = 2
            name = "pi"
            root = "pi"

            [detect]
            argv0 = ["pi"]
            argv_contains = ["pi"]
            argv_excludes = ["--print"]
            "#,
        )
        .unwrap();
        let adapter = ManifestAdapter::new(manifest);
        let info = PaneInfo {
            argv: vec![
                "/usr/bin/pi".into(),
                "--provider".into(),
                "fake".into(),
                "--print".into(),
                "say hello".into(),
            ],
            comm: "pi".into(),
            first_bytes: Vec::new(),
            env: BTreeMap::new(),
        };

        assert!(adapter.detect(&info).await <= f32::EPSILON);
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

    fn snap(content: &str) -> EngineSnapshot {
        let lines: Vec<&str> = content.split('\n').collect();
        let cols = u16::try_from(
            lines
                .iter()
                .map(|l| l.chars().count())
                .max()
                .unwrap_or(1)
                .max(1),
        )
        .expect("test fixture width fits u16");
        let rows = u16::try_from(lines.len()).expect("test fixture height fits u16");
        let total = usize::from(cols) * usize::from(rows);
        let mut cells = vec![
            Cell {
                ch: " ".into(),
                width: 1,
                fg: 0,
                bg: 0,
                attrs: 0,
            };
            total
        ];
        for (row, line) in lines.iter().enumerate() {
            for (col, ch) in line.chars().enumerate() {
                cells[row * usize::from(cols) + col] = Cell {
                    ch: ch.to_string(),
                    width: 1,
                    fg: 0,
                    bg: 0,
                    attrs: 0,
                };
            }
        }
        EngineSnapshot {
            cursor_visible: true,
            title: None,
            grid: CellGrid {
                cols,
                rows,
                cells,
                cursor: (rows.saturating_sub(1), 0),
            },
            modes: ModeFlags::default(),
            sequence: 1,
        }
    }
}
