//! `Pane` value + per-session `Registry`.
//!
//! A `Pane` ties together the three things the daemon owns per PTY: the
//! mutation-emitting `Engine`, the `PtyChild` driving it, and the metadata
//! the CLI cares about (argv, spawn time, geometry).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use agent_tui_adapter::Adapter;
use agent_tui_engine::Engine;
use agent_tui_protocol::{ErrorBody, ErrorCode, PaneId, Response};
use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, RwLock};

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
    /// Attached per-program adapter (`generic` as fallback). Selected by the
    /// registry's Detect lifecycle at spawn time. Swappable so the
    /// first-bytes re-detection pass can upgrade `generic` → real adapter
    /// once the child has emitted enough output.
    pub adapter: tokio::sync::RwLock<Arc<dyn Adapter>>,
}

impl Pane {
    /// Read the currently-attached adapter. Clones the Arc so callers don't
    /// hold the lock.
    pub async fn adapter(&self) -> Arc<dyn Adapter> {
        self.adapter.read().await.clone()
    }

    /// Swap in a new adapter; returns the previous one.
    pub async fn set_adapter(&self, adapter: Arc<dyn Adapter>) -> Arc<dyn Adapter> {
        let mut guard = self.adapter.write().await;
        std::mem::replace(&mut *guard, adapter)
    }
}

/// Tri-state focus model. `Auto` is the historical 0/1/many resolution path;
/// `Focused` is an explicit selection; `Held` means a focused pane died and
/// the user owes the daemon a fresh `pane focus` call before no-`--pane`
/// commands resume working (the user picked "explicit refocus required").
#[derive(Debug, Default)]
enum FocusState {
    #[default]
    Auto,
    Focused(PaneId),
    Held,
}

/// Per-session pane registry. Allocates monotonic `p<N>` ids that are never
/// reused even after `Die`.
pub struct Registry {
    next_id: AtomicU64,
    panes: RwLock<HashMap<PaneId, Arc<Pane>>>,
    focused: Mutex<FocusState>,
}

impl Registry {
    /// Construct an empty registry. The next allocated id is `p1`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            panes: RwLock::new(HashMap::new()),
            focused: Mutex::new(FocusState::Auto),
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

    /// Currently-focused pane id, if any (Focused state only).
    pub async fn focused(&self) -> Option<PaneId> {
        match &*self.focused.lock().await {
            FocusState::Focused(id) => Some(id.clone()),
            FocusState::Auto | FocusState::Held => None,
        }
    }

    /// Set the focused pane. The pane must exist in the registry, or this
    /// returns `false`. Passing `None` clears the focus back to `Auto`.
    pub async fn set_focused(&self, id: Option<PaneId>) -> bool {
        let mut focused = self.focused.lock().await;
        if let Some(new_id) = id {
            if !self.panes.read().await.contains_key(&new_id) {
                return false;
            }
            *focused = FocusState::Focused(new_id);
        } else {
            *focused = FocusState::Auto;
        }
        true
    }

    /// Called by the `die` handler when the focused pane is killed. Demotes
    /// the focus state to `Held` so no-`--pane` commands block until the
    /// user re-focuses explicitly.
    pub async fn mark_focus_held_if(&self, killed: &PaneId) {
        let mut focused = self.focused.lock().await;
        if let FocusState::Focused(id) = &*focused
            && id == killed
        {
            *focused = FocusState::Held;
        }
    }
}

/// Resolve a no-`--pane` command to a concrete `Arc<Pane>`.
///
/// Precedence:
///  1. Explicit `pane` argument wins if it resolves.
///  2. Explicitly-focused pane wins if it still exists.
///  3. If exactly one pane is live, use it.
///  4. Otherwise, return `NO_ACTIVE_PANE`.
pub async fn resolve_focused(
    registry: &Arc<Registry>,
    pane: Option<PaneId>,
) -> Result<Arc<Pane>, Response> {
    if let Some(id) = pane {
        return registry.get(&id).await.ok_or_else(|| {
            Response::err(ErrorBody::new(
                ErrorCode::NoActivePane,
                format!("pane {id} not found"),
                "call list to see live panes",
            ))
        });
    }
    // Explicit focus path.
    let state = registry.focused.lock().await;
    match &*state {
        FocusState::Focused(id) => {
            let id = id.clone();
            drop(state);
            return registry.get(&id).await.ok_or_else(|| {
                Response::err(ErrorBody::new(
                    ErrorCode::NoActivePane,
                    format!("focused pane {id} no longer exists"),
                    "call `pane focus <id>` to re-pick a target",
                ))
            });
        }
        FocusState::Held => {
            return Err(Response::err(ErrorBody::new(
                ErrorCode::NoActivePane,
                "previous focused pane died; explicit refocus required",
                "call `pane focus <id>` (or pass --pane) to continue",
            )));
        }
        FocusState::Auto => {}
    }
    drop(state);

    let list = registry.list().await;
    match list.len() {
        1 => registry.get(&list[0].id).await.ok_or_else(|| {
            Response::err(ErrorBody::new(
                ErrorCode::NoActivePane,
                "pane disappeared",
                "retry",
            ))
        }),
        0 => Err(Response::err(ErrorBody::new(
            ErrorCode::NoActivePane,
            "no panes",
            "spawn a pane first",
        ))),
        _ => Err(Response::err(ErrorBody::new(
            ErrorCode::NoActivePane,
            "multiple panes; --pane or `pane focus <id>` required",
            "pass --pane p<N> or call `agent-tui pane focus <id>`",
        ))),
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
