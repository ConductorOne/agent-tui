//! Per-pane (sequence → snapshot-hash) ring buffer.
//!
//! Backs `wait --hash <h>` — the CLI passes a hash returned by a prior
//! `snapshot`, the daemon looks up the matching sequence, and the wait
//! proceeds as if it had been `wait --since <seq>`.
//!
//! Capacity is fixed (256 entries per pane); older entries are evicted
//! oldest-first. The snapshot handler records into the window after every
//! successful snapshot.

use std::collections::HashMap;
use std::collections::VecDeque;

use agent_tui_protocol::{PaneId, Sequence};
use tokio::sync::RwLock;

const WINDOW_SIZE: usize = 256;

/// Bounded per-pane (seq, hash) ring.
#[derive(Default)]
pub struct HashWindow {
    inner: RwLock<HashMap<PaneId, VecDeque<(Sequence, String)>>>,
}

impl HashWindow {
    /// Build an empty window.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `(seq, hash)` for a pane. Evicts the oldest entry past
    /// [`WINDOW_SIZE`].
    pub async fn record(&self, pane: &PaneId, seq: Sequence, hash: String) {
        let mut g = self.inner.write().await;
        let ring = g.entry(pane.clone()).or_default();
        // If we already have the same (seq, hash) at the tail, skip — snapshot
        // calls at the same sequence (no intervening mutation) produce
        // identical hashes; no need to fill the window with duplicates.
        if ring.back().is_some_and(|t| t.0 == seq) {
            return;
        }
        ring.push_back((seq, hash));
        while ring.len() > WINDOW_SIZE {
            ring.pop_front();
        }
    }

    /// Look up the sequence for a hash, if it's still in the window.
    pub async fn lookup(&self, pane: &PaneId, hash: &str) -> Option<Sequence> {
        let g = self.inner.read().await;
        g.get(pane)
            .and_then(|ring| ring.iter().rev().find(|(_, h)| h == hash))
            .map(|(s, _)| *s)
    }

    /// Drop a pane's entries on `Die`.
    pub async fn drop_pane(&self, pane: &PaneId) {
        self.inner.write().await.remove(pane);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(s: &str) -> PaneId {
        PaneId(s.into())
    }

    #[tokio::test]
    async fn lookup_returns_recorded_sequence() {
        let w = HashWindow::new();
        w.record(&pid("p1"), 7, "abc".into()).await;
        assert_eq!(w.lookup(&pid("p1"), "abc").await, Some(7));
        assert_eq!(w.lookup(&pid("p1"), "missing").await, None);
        assert_eq!(w.lookup(&pid("p2"), "abc").await, None);
    }

    #[tokio::test]
    async fn ring_evicts_oldest() {
        let w = HashWindow::new();
        for i in 0..(WINDOW_SIZE as u64 + 50) {
            w.record(&pid("p"), i, format!("h{i}")).await;
        }
        // h0 is gone.
        assert_eq!(w.lookup(&pid("p"), "h0").await, None);
        // h50 should still be present (since we only added 50 past capacity).
        assert!(w.lookup(&pid("p"), "h50").await.is_some());
    }

    #[tokio::test]
    async fn duplicate_seq_is_noop() {
        let w = HashWindow::new();
        w.record(&pid("p1"), 5, "abc".into()).await;
        w.record(&pid("p1"), 5, "abc".into()).await;
        w.record(&pid("p1"), 5, "abc".into()).await;
        let g = w.inner.read().await;
        assert_eq!(g.get(&pid("p1")).unwrap().len(), 1);
    }
}
