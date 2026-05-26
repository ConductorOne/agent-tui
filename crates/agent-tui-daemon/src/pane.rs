//! `Pane` value + per-session `Registry`.
//!
//! A `Pane` ties together the three things the daemon owns per PTY: the
//! mutation-emitting `Engine`, the `PtyChild` driving it, and the metadata
//! the CLI cares about (argv, spawn time, geometry).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use agent_tui_engine::Engine;
use agent_tui_protocol::PaneId;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use crate::pty::PtyChild;

/// One PTY-backed pane: an engine driven by output from a child process.
pub struct Pane {
    /// Stable per-session pane id (e.g. `p1`). Never reused after `Die`.
    pub id: PaneId,
    /// argv used to launch the pane's child.
    pub argv: Vec<String>,
    /// Wall-clock spawn time.
    pub spawned_at: DateTime<Utc>,
    /// Geometry — columns.
    pub cols: u16,
    /// Geometry — rows.
    pub rows: u16,
    /// VT engine consuming PTY output.
    pub engine: Arc<dyn Engine>,
    /// PTY master + child handle.
    pub pty: PtyChild,
}

/// Per-session pane registry. Allocates monotonic `p<N>` ids that are never
/// reused even after `Die`.
pub struct Registry {
    next_id: AtomicU64,
    panes: RwLock<HashMap<PaneId, Arc<Pane>>>,
}

impl Registry {
    /// Construct an empty registry. The next allocated id is `p1`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            panes: RwLock::new(HashMap::new()),
        }
    }

    /// Reserve the next `p<N>` id. Always increases.
    pub fn alloc_id(&self) -> PaneId {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        PaneId::from_counter(n)
    }

    /// Insert a newly-constructed pane. The caller already owns the
    /// `PaneId` (via `alloc_id`).
    pub async fn insert(&self, pane: Pane) -> Arc<Pane> {
        let arc = Arc::new(pane);
        self.panes.write().await.insert(arc.id.clone(), arc.clone());
        arc
    }

    /// Look up a pane by id.
    pub async fn get(&self, id: &PaneId) -> Option<Arc<Pane>> {
        self.panes.read().await.get(id).cloned()
    }

    /// Remove a pane by id, returning the previous entry if present.
    pub async fn remove(&self, id: &PaneId) -> Option<Arc<Pane>> {
        self.panes.write().await.remove(id)
    }

    /// Snapshot of `(id, argv, spawned_at, cols, rows)` for every live pane.
    pub async fn list(&self) -> Vec<PaneSummary> {
        self.panes
            .read()
            .await
            .values()
            .map(|p| PaneSummary {
                id: p.id.clone(),
                argv: p.argv.clone(),
                spawned_at: p.spawned_at,
                cols: p.cols,
                rows: p.rows,
            })
            .collect()
    }

    /// Number of live panes.
    pub async fn count(&self) -> usize {
        self.panes.read().await.len()
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

/// Lightweight pane descriptor for the `list` command.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PaneSummary {
    /// Pane id.
    pub id: PaneId,
    /// argv used to launch the child.
    pub argv: Vec<String>,
    /// Wall-clock spawn time.
    pub spawned_at: DateTime<Utc>,
    /// Geometry — columns.
    pub cols: u16,
    /// Geometry — rows.
    pub rows: u16,
}
