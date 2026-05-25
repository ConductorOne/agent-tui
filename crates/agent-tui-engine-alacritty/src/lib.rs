//! `alacritty-terminal`-backed [`Engine`] implementation.
//!
//! Built as the lean alternative to `agent-tui-engine-wezterm`. The actual
//! integration lands in P5 (`docs/RFC.md` §17). This file holds the type
//! declaration and a placeholder constructor so the workspace stays green.

#![forbid(unsafe_code)]

use agent_tui_engine::{Engine, EngineError, EngineSnapshot, MutationStream};

/// Placeholder `alacritty-terminal`-backed engine.
///
/// Implementation lands in P5. Today this returns an error from any call;
/// callers should select the `wezterm` engine via `--engine wezterm` (the
/// default).
pub struct AlacrittyEngine;

impl AlacrittyEngine {
    /// Construct an alacritty engine. P5 will accept the same `(cols, rows)`
    /// constructor as the wezterm placeholder.
    #[must_use]
    pub fn new(_cols: u16, _rows: u16) -> Self {
        Self
    }
}

impl Engine for AlacrittyEngine {
    fn feed(&self, _bytes: &[u8]) -> Result<(), EngineError> {
        Err(EngineError::Refused(
            "alacritty engine not yet implemented; pass --engine wezterm".into(),
        ))
    }

    fn snapshot(&self) -> EngineSnapshot {
        // Returning an empty snapshot from a placeholder is acceptable;
        // callers should not be invoking us yet.
        EngineSnapshot {
            grid: agent_tui_engine::CellGrid {
                cols: 0,
                rows: 0,
                cells: Vec::new(),
                cursor: (0, 0),
            },
            modes: agent_tui_engine::ModeFlags::default(),
            sequence: 0,
        }
    }

    fn resize(&self, _cols: u16, _rows: u16) -> Result<(), EngineError> {
        Err(EngineError::Refused(
            "alacritty engine not yet implemented".into(),
        ))
    }

    fn subscribe(&self) -> MutationStream {
        let (tx, _) = tokio::sync::broadcast::channel(1);
        let _ = tx; // keep the channel alive via the receiver
        tx.subscribe()
    }
}
