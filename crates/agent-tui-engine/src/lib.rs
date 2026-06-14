//! Engine trait — the abstraction over the underlying headless VT crate.
//!
//! All daemon code talks to this trait; engine-specific code lives in the
//! `agent-tui-engine-wezterm` and `agent-tui-engine-alacritty` crates.
//! When `libghostty-vt` tags stable a third implementor can land without
//! touching daemon code.
//!
//! See `docs/design/RFC.md` §3.3.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub use agent_tui_protocol::{Cell, CellGrid, ModeFlags, Sequence};

/// Engine errors. Kept small — implementors should map into a common shape.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The engine implementation refused or could not satisfy the request.
    #[error("engine refused: {0}")]
    Refused(String),
    /// I/O or runtime failure inside the engine.
    #[error("engine io: {0}")]
    Io(#[from] std::io::Error),
}

/// What changed in the engine state for the most recent [`MutationEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationKind {
    /// New output bytes were fed into the engine.
    Output,
    /// Input bytes were emitted by `feed_input` (rare — most input is sent
    /// straight to the PTY).
    Input,
    /// A resize was applied.
    Resize,
    /// A DEC/ANSI mode flag changed (alt-screen toggle, bracketed paste,
    /// KKP push/pop, mouse mode change).
    ModeChange,
    /// The child rang the terminal bell (BEL / `\x07`).
    Bell,
}

/// Emitted on every mutation, *after* the change is observable via
/// [`Engine::snapshot`]. Sequence numbers are monotonic per-pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationEvent {
    /// Monotonic sequence number; equals the snapshot's sequence post-event.
    pub sequence: Sequence,
    /// What kind of mutation it was.
    pub kind: MutationKind,
}

/// Owned, immutable snapshot of an engine's state at one moment.
///
/// Returned by [`Engine::snapshot`]; callers do not hold any engine lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSnapshot {
    /// Cell grid + cursor.
    pub grid: CellGrid,
    /// DEC/ANSI mode flags.
    pub modes: ModeFlags,
    /// Sequence number that produced this snapshot.
    pub sequence: Sequence,
    /// Whether the cursor is visible (DECTCEM, DECSET 25). Defaults to
    /// `true` — terminals show the cursor unless the program hides it.
    pub cursor_visible: bool,
    /// Window title most recently set via OSC 0/2, if any.
    pub title: Option<String>,
}

impl EngineSnapshot {
    /// Compute the canonical SHA-256 hash of the visible cell grid.
    ///
    /// The encoding is documented in `docs/design/RFC.md` §4.4: row-major
    /// `(ch, fg, bg, attrs, width)` packing followed by alt-screen flag
    /// and cursor position. The output is hex-encoded (lowercase, no `0x`).
    #[must_use]
    pub fn canonical_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(self.grid.cols.to_le_bytes());
        h.update(self.grid.rows.to_le_bytes());
        for c in &self.grid.cells {
            h.update(c.ch.as_bytes());
            h.update([0u8]); // ch terminator
            h.update([c.width]);
            h.update(c.fg.to_le_bytes());
            h.update(c.bg.to_le_bytes());
            h.update([c.attrs]);
        }
        h.update([u8::from(self.modes.alt_screen)]);
        h.update(self.grid.cursor.0.to_le_bytes());
        h.update(self.grid.cursor.1.to_le_bytes());
        hex::encode(h.finalize())
    }
}

/// Subscription handle returned by [`Engine::subscribe`].
pub type MutationStream = tokio::sync::broadcast::Receiver<MutationEvent>;

/// The headless VT engine abstraction.
///
/// Implementations live in `agent-tui-engine-wezterm` (default) and
/// `agent-tui-engine-alacritty` (lean). The daemon uses this trait
/// exclusively — engine-specific types never appear in daemon code.
pub trait Engine: Send + Sync {
    /// Feed PTY output bytes into the engine. May trigger a mutation event.
    ///
    /// # Errors
    /// Engine implementations report parse / runtime failures via
    /// [`EngineError`] but are encouraged to keep `feed` infallible where
    /// possible.
    fn feed(&self, bytes: &[u8]) -> Result<(), EngineError>;

    /// Take an atomic, owned snapshot of the current grid + cursor + modes.
    fn snapshot(&self) -> EngineSnapshot;

    /// The engine's current grid geometry as `(cols, rows)`.
    ///
    /// Reflects the live size — the latest [`Engine::resize`] — and is the
    /// same dimensions a `snapshot` exposes via its grid, but without cloning
    /// the cell buffer. `list` uses this so it agrees with reality after a
    /// resize instead of reporting the spawn-time geometry.
    fn dimensions(&self) -> (u16, u16);

    /// Apply a resize.
    ///
    /// # Errors
    /// Returns `EngineError::Refused` if the engine does not support the
    /// requested size.
    fn resize(&self, cols: u16, rows: u16) -> Result<(), EngineError>;

    /// Subscribe to mutation events. Each subscriber receives every event
    /// emitted after subscription.
    fn subscribe(&self) -> MutationStream;
}

/// Convenient alias for boxed dyn engines (the daemon stores one per pane).
pub type DynEngine = Arc<dyn Engine>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_hash_is_deterministic() {
        let snap = EngineSnapshot {
            cursor_visible: true,
            title: None,
            grid: CellGrid {
                cols: 1,
                rows: 1,
                cells: vec![Cell {
                    ch: "A".to_string(),
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
        let h1 = snap.canonical_hash();
        let h2 = snap.canonical_hash();
        assert_eq!(h1, h2, "hash must be deterministic");
        assert_eq!(h1.len(), 64, "hex SHA-256 = 64 chars");
    }

    #[test]
    fn canonical_hash_distinguishes_cells() {
        let mk = |ch: &str| EngineSnapshot {
            cursor_visible: true,
            title: None,
            grid: CellGrid {
                cols: 1,
                rows: 1,
                cells: vec![Cell {
                    ch: ch.to_string(),
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
        assert_ne!(mk("A").canonical_hash(), mk("B").canonical_hash());
    }
}
