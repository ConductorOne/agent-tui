//! Snapshot payload types — what the daemon returns to a `snapshot` request.
//!
//! See `docs/RFC.md` §5 (envelope) and §7 (observation layer).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{Generation, PaneId, Sequence};

/// Classification of the pane's current state. Drives both UI hints and
/// auto-handling (e.g. `password_prompt` is the only state where
/// `auth use` may inject).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneState {
    /// A shell prompt is showing (heuristic or OSC 133 confirmed).
    Shell,
    /// A foreground command is running (no alt-screen).
    Running,
    /// A full-screen TUI is active (alt-screen 1049 on).
    AltScreenTui,
    /// A `less`/`more`-style pager is active.
    Pager,
    /// An editor (nvim/vim) is active. Distinguished from alt-screen TUI by
    /// adapter detection.
    Editor,
    /// A REPL (python/node/...) is at its prompt.
    Repl,
    /// A `Password:` or `passphrase` prompt is on screen.
    PasswordPrompt,
    /// A `[Y/n]` or "Continue?" confirmation is on screen.
    Confirm,
    /// A selection menu (`fzf`, `select`) is on screen.
    Selection,
    /// We don't know.
    Unknown,
}

/// DEC/ANSI mode flags relevant to agents.
///
/// The clippy warning about "too many bools" is intentional here — each
/// flag is an independent DEC/ANSI mode, not part of a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeFlags {
    /// Alt-screen buffer active (DECSET 1049 / 1047 / 47).
    pub alt_screen: bool,
    /// Bracketed paste mode (DECSET 2004).
    pub bracketed_paste: bool,
    /// Mouse-tracking mode 1003 active.
    pub mouse_1003: bool,
    /// Kitty keyboard protocol pushed.
    pub kkp_active: bool,
    /// Kitty keyboard protocol flag bitmask if active.
    pub kkp_flags: u32,
}

/// A single visible cell. Multi-column glyphs (CJK) emit a cell with
/// `width = 2` followed by a continuation cell with `width = 0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    /// The glyph cluster (typically one char; may be a grapheme cluster).
    pub ch: String,
    /// Display width (0 = continuation of a wide cell, 1 = normal, 2 = wide).
    pub width: u8,
    /// Foreground palette index or 24-bit color.
    pub fg: u32,
    /// Background palette index or 24-bit color.
    pub bg: u32,
    /// Bit-packed attributes: bold/italic/underline/inverse/strikethrough.
    pub attrs: u8,
}

/// Cell grid for a pane. `cells` is laid out row-major, length `cols * rows`.
///
/// For wire efficiency, callers usually prefer the RLE-compressed
/// representation in [`CellGridRle`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellGrid {
    /// Number of columns.
    pub cols: u16,
    /// Number of rows.
    pub rows: u16,
    /// Cells in row-major order, length `cols * rows`.
    pub cells: Vec<Cell>,
    /// `(row, col)` of the cursor.
    pub cursor: (u16, u16),
}

/// RLE-compressed cell grid carried in `snapshot --mode cells` JSON outputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellGridRle {
    /// Number of columns.
    pub cols: u16,
    /// Number of rows.
    pub rows: u16,
    /// Base64-packed RLE-compressed cell records. Format defined in
    /// `docs/RFC.md` §7.2.
    pub rows_b64: Vec<String>,
    /// Palette descriptor.
    pub palette: serde_json::Value,
    /// `(row, col)` of the cursor.
    pub cursor: (u16, u16),
}

/// A semantic outline node. Refs into focused rows are first-class via
/// nested `children`.
///
/// Example tree (k9s):
///
/// ```text
/// @e3 [list focused=4]
///     @e3.1 "coredns-abc       Running    2/2   3d"
///     @e3.2 "etcd-cp-1         Running    1/1   12d"
///     @e3.3 "kube-proxy-xyz    Running    1/1   7d"
///     @e3.4 "metrics-server-1  CrashLoop  0/1   3h"   <-- focused, red
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineNode {
    /// Ref handle, e.g. `@e3` or `@e3.4`.
    pub r#ref: String,
    /// ARIA-like role (`button`, `textbox`, `heading`, `list`, ...).
    pub role: String,
    /// Accessible name / text content (may be empty).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Optional value (input value, heading level, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Whether this node currently has focus.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub focused: bool,
    /// Row/column where this node anchors (display columns, width-aware).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<(u16, u16)>,
    /// Per-node pane state, when meaningful (e.g. a focused sub-pane
    /// inside a multiplexer). When unset, consult the snapshot-level
    /// `state` field instead. See `docs/addressing-rfc.md` §2.5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<PaneState>,
    /// `true` if the ref is bound by an adapter-durable identifier
    /// (i.e. survives re-renders). Mirrors the binding kind in the
    /// snapshot's `refs` map; inlined here so selectors can filter
    /// without joining against `refs`. See `docs/addressing-rfc.md`
    /// §2.1 "Stability rule".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub durable: bool,
    /// Nested children (for lists, forms, tables).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<OutlineNode>,
}

/// Outline returned in the default `snapshot --mode outline`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outline {
    /// Adapter that produced this outline (`generic`, `nvim`, `tmux`, ...).
    pub adapter: String,
    /// Top-level outline nodes.
    pub nodes: Vec<OutlineNode>,
}

/// How a ref is bound — `Durable` survives small re-renders within a pane;
/// `Generic` is only valid within its snapshot generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RefBinding {
    /// Bound to an adapter-durable identifier.
    Durable {
        /// Identifier scheme name (e.g. `"nvim.buffer"`, `"tmux.pane"`).
        scheme: String,
        /// Adapter-defined identifier payload.
        id: serde_json::Value,
    },
    /// Fallback binding: outline traversal position. Valid only within the
    /// snapshot generation that produced it.
    Generic {
        /// Row anchor (display row).
        row: u16,
        /// Column anchor (display column).
        col: u16,
        /// Role tag (e.g. `"button"`).
        role: String,
    },
}

/// A single ref entry surfaced in the snapshot's `refs` map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ref {
    /// Display role tag.
    pub role: String,
    /// Accessible name (may be empty).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Binding strategy (see [`RefBinding`]).
    pub binding: RefBinding,
}

/// Full snapshot payload returned from `snapshot`.
///
/// Carried inside [`crate::Response`]'s `data` field, this is the canonical
/// view of a pane at a moment in time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Pane this snapshot is for.
    pub pane: PaneId,
    /// Pane state classifier.
    pub state: PaneState,
    /// Generation at which this snapshot was taken.
    pub generation: Generation,
    /// Mutation sequence at which this snapshot was taken.
    pub sequence: Sequence,
    /// SHA-256 of the canonical cell-grid encoding, hex-encoded.
    pub hash: String,
    /// Adapter outline (always present for `--mode outline|hybrid`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline: Option<Outline>,
    /// RLE-compressed cell grid (present for `--mode cells|hybrid`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cells: Option<CellGridRle>,
    /// Visible cells flattened to a plain UTF-8 string, rows joined
    /// with `\n` and trailing whitespace trimmed (present for
    /// `--mode text|hybrid`). The most agent-friendly form when the
    /// pane is unstructured text output and outline/cells are
    /// overkill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Mode flags at snapshot time.
    pub modes: ModeFlags,
    /// Ref map. Keys are the ref handles (`@e1`, `@e3.4`, …).
    #[serde(default)]
    pub refs: BTreeMap<String, Ref>,
}
