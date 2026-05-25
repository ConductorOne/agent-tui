//! `wezterm-term`-backed [`Engine`] implementation.
//!
//! The actual `wezterm-term` integration is the first deliverable of P0
//! (see `docs/RFC.md` §17). This file ships a placeholder that:
//!
//! - validates the trait-bound layout end-to-end so the daemon can wire up
//!   an engine even before the substrate lands, and
//! - documents the API surface we expect `wezterm-term` to bind to.
//!
//! The placeholder is intentionally non-public: switching from placeholder to
//! real `wezterm-term` is the only change needed at the daemon callsite.

#![forbid(unsafe_code)]

use std::sync::Mutex;

use agent_tui_engine::{
    Cell, CellGrid, Engine, EngineError, EngineSnapshot, ModeFlags, MutationEvent, MutationKind,
    MutationStream, Sequence,
};
use tokio::sync::broadcast;

/// A trivial in-memory engine satisfying the [`Engine`] contract.
///
/// Holds an empty cell grid; every `feed` bumps the sequence and emits an
/// [`MutationEvent`]. Useful for daemon-level tests and for keeping the
/// workspace green until `wezterm-term` is wired in.
pub struct PlaceholderEngine {
    state: Mutex<State>,
    events: broadcast::Sender<MutationEvent>,
}

struct State {
    cols: u16,
    rows: u16,
    sequence: Sequence,
    modes: ModeFlags,
}

impl PlaceholderEngine {
    /// Construct a placeholder engine with the given initial geometry.
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            state: Mutex::new(State {
                cols,
                rows,
                sequence: 0,
                modes: ModeFlags::default(),
            }),
            events,
        }
    }

    fn make_snapshot(state: &State) -> EngineSnapshot {
        let total = usize::from(state.cols) * usize::from(state.rows);
        let cells = (0..total)
            .map(|_| Cell {
                ch: " ".to_string(),
                width: 1,
                fg: 0,
                bg: 0,
                attrs: 0,
            })
            .collect();
        EngineSnapshot {
            grid: CellGrid {
                cols: state.cols,
                rows: state.rows,
                cells,
                cursor: (0, 0),
            },
            modes: state.modes.clone(),
            sequence: state.sequence,
        }
    }
}

impl Engine for PlaceholderEngine {
    fn feed(&self, _bytes: &[u8]) -> Result<(), EngineError> {
        let mut s = self
            .state
            .lock()
            .map_err(|e| EngineError::Refused(format!("poisoned: {e}")))?;
        s.sequence = s.sequence.saturating_add(1);
        let evt = MutationEvent {
            sequence: s.sequence,
            kind: MutationKind::Output,
        };
        // Send failures (no subscribers) are not errors at the engine layer.
        let _ = self.events.send(evt);
        Ok(())
    }

    fn snapshot(&self) -> EngineSnapshot {
        let s = self.state.lock().expect("engine state lock poisoned");
        Self::make_snapshot(&s)
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), EngineError> {
        let mut s = self
            .state
            .lock()
            .map_err(|e| EngineError::Refused(format!("poisoned: {e}")))?;
        s.cols = cols;
        s.rows = rows;
        s.sequence = s.sequence.saturating_add(1);
        let _ = self.events.send(MutationEvent {
            sequence: s.sequence,
            kind: MutationKind::Resize,
        });
        Ok(())
    }

    fn subscribe(&self) -> MutationStream {
        self.events.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn feed_bumps_sequence_and_emits() {
        let eng = PlaceholderEngine::new(80, 24);
        let mut sub = eng.subscribe();
        let s0 = eng.snapshot().sequence;
        eng.feed(b"hello").expect("feed ok");
        let evt = sub.recv().await.expect("event arrived");
        assert_eq!(evt.sequence, s0 + 1);
        assert_eq!(evt.kind, MutationKind::Output);
        assert_eq!(eng.snapshot().sequence, s0 + 1);
    }

    #[tokio::test]
    async fn resize_emits_resize_event() {
        let eng = PlaceholderEngine::new(80, 24);
        let mut sub = eng.subscribe();
        eng.resize(132, 40).expect("resize ok");
        let evt = sub.recv().await.expect("event arrived");
        assert_eq!(evt.kind, MutationKind::Resize);
    }
}
