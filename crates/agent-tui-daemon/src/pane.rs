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

/// Quiet-period a streaming verb waits after the child exits before treating
/// the output ring as fully drained, when the reader thread hasn't reported EOF.
///
/// This backs the Windows-ConPTY branch of [`Pane::exit_drained`]: the daemon
/// holds the ConPTY master for the pane's life, so conhost keeps the output
/// pipe's write end open and the blocking reader never observes EOF (so
/// [`PtyChild::reader_finished`] never flips). ConPTY emits nothing after the
/// child exits, so `last_output_ms_ago` only grows; once it passes this grace
/// the ring is settled. Sized at ~3× the server's 50ms `STREAM_IDLE_TICK`:
/// long enough for conhost to flush a child's final output, short enough to
/// keep `events` / `tail --follow` responsive at child exit. On Unix the grace
/// is never consulted — `reader_finished()` flips the instant the child exits.
const DRAIN_GRACE_MS: u64 = 150;

/// One PTY-backed pane: an engine driven by output from a child process.
pub struct Pane {
    /// Stable per-session pane id (e.g. `p1`). Never reused after `Die`.
    pub id: PaneId,
    /// argv used to launch the pane's child.
    pub argv: Vec<String>,
    /// Wall-clock spawn time.
    pub spawned_at: DateTime<Utc>,
    /// VT engine consuming PTY output. Source of truth for the pane's live
    /// geometry (see [`Registry::list`]).
    pub engine: Arc<dyn Engine>,
    /// PTY master + child handle.
    pub pty: PtyChild,
    /// Attached per-program adapter (`generic` as fallback). Selected by the
    /// registry's Detect lifecycle at spawn time. Swappable so the
    /// first-bytes re-detection pass can upgrade `generic` → real adapter
    /// once the child has emitted enough output.
    pub adapter: tokio::sync::RwLock<Arc<dyn Adapter>>,
    /// Single-writer lease. `Some(token)` → only that token may write
    /// (`type`/`press`/`send-ansi`/`stdin`); `None` → free-for-all (the
    /// historical default, so single-driver users are unaffected). Acquired by
    /// `attach --write-lease`, auto-released when that attacher disconnects.
    pub(crate) lease: std::sync::Mutex<Option<uuid::Uuid>>,
    /// Remembered shell-style exit code once the child has terminated (signal
    /// death → 128+sig, via `map_exit`). `None` while running. Set lazily the
    /// first time the daemon observes the child exit (any follower poll, `die`,
    /// or `list`), making a terminal pane **retained**: late observers and
    /// `list` read the remembered outcome instead of "no such pane".
    pub(crate) last_exit: std::sync::Mutex<Option<i32>>,
}

impl Pane {
    /// Read the currently-attached adapter. Clones the Arc so callers don't
    /// hold the lock.
    pub async fn adapter(&self) -> Arc<dyn Adapter> {
        self.adapter.read().await.clone()
    }

    /// Observe the child's exit, memoizing the shell-style code the first time
    /// it's seen. Returns the remembered code (or freshly-observed one), or
    /// `None` while the child is still running. Idempotent and authoritative:
    /// once set, it stands even after the OS child handle is reaped.
    #[must_use]
    pub fn poll_exit(&self) -> Option<i32> {
        if let Some(code) = *self.last_exit.lock().expect("last_exit poisoned") {
            return Some(code);
        }
        // try_exit_code returns the `map_exit`-mapped code (128+sig on signal
        // death) for our std::process-backed children.
        if let Ok(Some(code)) = self.pty.try_exit_code() {
            let code = i32::try_from(code).unwrap_or(1);
            *self.last_exit.lock().expect("last_exit poisoned") = Some(code);
            return Some(code);
        }
        None
    }

    /// The remembered exit code without polling the OS (read-only).
    #[must_use]
    pub fn remembered_exit(&self) -> Option<i32> {
        *self.last_exit.lock().expect("last_exit poisoned")
    }

    /// Whether this pane is terminal-retained (its child has exited and the
    /// code is remembered).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.poll_exit().is_some()
    }

    /// Terminal-drain gate shared by the streaming verbs (`events`,
    /// `tail --follow`, `attach`): true once the child has exited **and** every
    /// trailing byte is settled in the output ring, so the stream can emit its
    /// terminal event (`child_exited` / `eof`) without dropping final output.
    ///
    /// "Settled" is satisfied either by the reader thread observing EOF
    /// ([`PtyChild::reader_finished`]) — the normal Unix case, where closing the
    /// slave EOFs the master the instant the child exits — **or** by the pane
    /// going quiet for [`DRAIN_GRACE_MS`]. The grace branch exists for Windows
    /// ConPTY, where the daemon-held master keeps conhost's output-pipe write
    /// end open so the blocking reader never sees EOF: ConPTY emits nothing
    /// after the child exits, so `last_output_ms_ago` only grows and, once past
    /// the grace, the ring is fully flushed. `last_output_ms_ago() == None`
    /// (child produced no output at all) also drains immediately — there is
    /// nothing to wait for. On Unix `reader_finished()` flips first, so the
    /// grace branch is never reached and behavior is byte-for-byte unchanged.
    #[must_use]
    pub fn exit_drained(&self) -> bool {
        self.poll_exit().is_some()
            && (self.pty.reader_finished()
                || self
                    .pty
                    .last_output_ms_ago()
                    .map_or(true, |ms| ms > DRAIN_GRACE_MS))
    }

    /// Swap in a new adapter; returns the previous one.
    pub async fn set_adapter(&self, adapter: Arc<dyn Adapter>) -> Arc<dyn Adapter> {
        let mut guard = self.adapter.write().await;
        std::mem::replace(&mut *guard, adapter)
    }

    /// Try to acquire the write-lease for `token`. Returns `Ok(())` if granted
    /// (lease was free, or `token` already holds it); `Err(holder)` if another
    /// token holds it.
    pub fn acquire_lease(&self, token: uuid::Uuid) -> Result<(), uuid::Uuid> {
        let mut g = self.lease.lock().expect("lease poisoned");
        match *g {
            Some(held) if held != token => Err(held),
            _ => {
                *g = Some(token);
                Ok(())
            }
        }
    }

    /// Release the write-lease if `token` holds it (no-op otherwise).
    pub fn release_lease(&self, token: uuid::Uuid) {
        let mut g = self.lease.lock().expect("lease poisoned");
        if *g == Some(token) {
            *g = None;
        }
    }

    /// Current lease holder, if any.
    #[must_use]
    pub fn lease_holder(&self) -> Option<uuid::Uuid> {
        *self.lease.lock().expect("lease poisoned")
    }

    /// Whether a write from `who` is permitted: allowed when the lease is free,
    /// or when `who` is the current holder.
    #[must_use]
    pub fn write_allowed(&self, who: Option<uuid::Uuid>) -> bool {
        match self.lease_holder() {
            None => true,
            Some(holder) => who == Some(holder),
        }
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

    /// Insert a pane reconstructed during an in-place upgrade adoption,
    /// advancing the id allocator so a later `spawn` never collides with the
    /// adopted id. `counter` is the numeric suffix of the adopted pane's id
    /// (`p<N>` → `N`).
    pub async fn adopt_insert(&self, pane: Pane, counter: u64) -> Arc<Pane> {
        // next_id is monotonic; make sure it sits strictly past the adopted id.
        let cur = self.next_id.load(Ordering::Relaxed);
        if counter >= cur {
            self.next_id.store(counter + 1, Ordering::Relaxed);
        }
        self.insert(pane).await
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
    ///
    /// `cols`/`rows` are the pane's **live** geometry, read from the engine
    /// grid (the same source `snapshot --mode cells` reflects), so `list`
    /// agrees with reality after a `resize` rather than reporting the
    /// spawn-time size.
    pub async fn list(&self) -> Vec<PaneSummary> {
        self.panes
            .read()
            .await
            .values()
            .map(|p| {
                let (cols, rows) = p.engine.dimensions();
                PaneSummary {
                    id: p.id.clone(),
                    argv: p.argv.clone(),
                    spawned_at: p.spawned_at,
                    cols,
                    rows,
                    // Surface the remembered exit code; `poll_exit` also lazily
                    // records it the first time `list` observes a finished child.
                    exit_code: p.poll_exit(),
                }
            })
            .collect()
    }

    /// Number of panes (live + terminal-retained).
    pub async fn count(&self) -> usize {
        self.panes.read().await.len()
    }

    /// Snapshot of every pane `Arc` (live + terminal-retained).
    pub async fn all_panes(&self) -> Vec<Arc<Pane>> {
        self.panes.read().await.values().cloned().collect()
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

    // Implicit (no-`--pane`) resolution. Terminal-retained panes (G5) linger in
    // the registry for late observers + `list`, but must NOT hijack implicit
    // resolution. Rules:
    //  - 0 panes → error.
    //  - exactly 1 live pane → it (the common single-driver case).
    //  - >1 live pane → genuinely ambiguous → require `--pane`.
    //  - 0 live panes (all terminal-retained) → the **most-recently-spawned**
    //    pane. This is the multi-turn flow (`spawn; die; spawn; …`, all
    //    no-`--pane`) where a turn's short-lived child (e.g. `pi --print`) has
    //    already exited by the time we resolve: target the *current* (latest)
    //    pane, whose final frame is retained, not the stale earlier one.
    let all = registry.all_panes().await;
    if all.is_empty() {
        return Err(Response::err(ErrorBody::new(
            ErrorCode::NoActivePane,
            "no panes",
            "spawn a pane first",
        )));
    }
    let mut live = all.iter().filter(|p| !p.is_terminal());
    match (live.next(), live.next()) {
        (Some(p), None) => Ok(p.clone()),
        (Some(_), Some(_)) => Err(Response::err(ErrorBody::new(
            ErrorCode::NoActivePane,
            "multiple live panes; --pane or `pane focus <id>` required",
            "pass --pane p<N> or call `agent-tui pane focus <id>`",
        ))),
        (None, _) => Ok(all
            .into_iter()
            .max_by_key(|p| p.spawned_at)
            .expect("registry non-empty")),
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
    /// Remembered shell-style exit code for a terminal-retained pane
    /// (signal death → 128+sig); `None` while the child is still running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}
