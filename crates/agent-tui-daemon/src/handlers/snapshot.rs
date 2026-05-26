//! `snapshot` command handler.
//!
//! v0.1.0 wires only `--mode outline` and emits a minimal "generic" outline:
//! one `@e1 [buffer]` node carrying the visible-grid text content. The full
//! mode matrix (`cells`/`hybrid`/`adapter`), per-program adapters, and the
//! real state classifier land in P1.

use std::collections::HashMap;
use std::sync::Arc;

use agent_tui_engine::EngineSnapshot;
use agent_tui_protocol::request::SnapshotMode;
use agent_tui_protocol::{
    ErrorBody, ErrorCode, Outline, OutlineNode, PaneId, PaneState, Ref, RefBinding, Response,
    Snapshot,
};

use crate::pane::{Pane, Registry};

/// Per-session generation counters keyed by pane id.
///
/// `generation` is *snapshot-driven*: it only ticks the first time a snapshot
/// observes a sequence higher than the last-snapshotted one. We move this
/// onto `Pane` once the per-pane concurrency machinery lands in P1.
#[derive(Default)]
pub struct GenerationTracker {
    inner: tokio::sync::Mutex<HashMap<PaneId, GenSlot>>,
}

#[derive(Default)]
struct GenSlot {
    last_seq: u64,
    generation: u64,
}

impl GenerationTracker {
    /// Compute the generation for a freshly-taken snapshot at `sequence`.
    pub async fn observe(&self, pane: &PaneId, sequence: u64) -> u64 {
        let mut g = self.inner.lock().await;
        let slot = g.entry(pane.clone()).or_default();
        if sequence > slot.last_seq {
            slot.generation = slot.generation.saturating_add(1);
            slot.last_seq = sequence;
        }
        slot.generation
    }
}

/// Take an outline-mode snapshot of the named pane (or the unique one when
/// `pane` is None).
pub async fn run(
    registry: &Arc<Registry>,
    generations: &Arc<GenerationTracker>,
    pane: Option<PaneId>,
    mode: SnapshotMode,
) -> Response {
    if mode != SnapshotMode::Outline {
        return Response::err(ErrorBody::new(
            ErrorCode::Internal,
            "only --mode outline is wired in v0.1.0",
            "cells/hybrid/adapter modes land in P1",
        ));
    }

    let pane_arc = match resolve(registry, pane).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let engine_snap = pane_arc.engine.snapshot();
    let generation = generations
        .observe(&pane_arc.id, engine_snap.sequence)
        .await;

    let snapshot = build_snapshot(&pane_arc, &engine_snap, generation);
    match serde_json::to_value(&snapshot) {
        Ok(v) => Response::ok(v),
        Err(e) => Response::err(ErrorBody::new(
            ErrorCode::Internal,
            format!("snapshot serialization failed: {e}"),
            "report a bug",
        )),
    }
}

fn build_snapshot(pane: &Pane, engine_snap: &EngineSnapshot, generation: u64) -> Snapshot {
    let hash = engine_snap.canonical_hash();
    let state = if engine_snap.modes.alt_screen {
        PaneState::AltScreenTui
    } else {
        PaneState::Unknown
    };

    let outline = generic_outline(engine_snap);
    let mut refs = std::collections::BTreeMap::new();
    refs.insert(
        "@e1".to_string(),
        Ref {
            role: "buffer".to_string(),
            name: String::new(),
            binding: RefBinding::Generic {
                row: 0,
                col: 0,
                role: "buffer".to_string(),
            },
        },
    );

    Snapshot {
        pane: pane.id.clone(),
        state,
        generation,
        sequence: engine_snap.sequence,
        hash,
        outline: Some(outline),
        cells: None,
        modes: engine_snap.modes.clone(),
        refs,
    }
}

/// Minimal outline: one `@e1 [buffer]` node whose `name` is the visible cell
/// grid rendered row-by-row with trailing whitespace per row stripped.
fn generic_outline(snap: &EngineSnapshot) -> Outline {
    let cols = usize::from(snap.grid.cols);
    let mut name = String::with_capacity(cols);
    for row in 0..usize::from(snap.grid.rows) {
        let mut line = String::with_capacity(cols);
        for col in 0..cols {
            let cell = &snap.grid.cells[row * cols + col];
            if cell.width == 0 {
                continue;
            }
            line.push_str(&cell.ch);
        }
        let trimmed = line.trim_end();
        if !name.is_empty() {
            name.push('\n');
        }
        name.push_str(trimmed);
    }
    // Drop trailing empty rows that contributed nothing.
    let stripped = name.trim_end_matches('\n').to_string();

    Outline {
        adapter: "generic".to_string(),
        nodes: vec![OutlineNode {
            r#ref: "@e1".to_string(),
            role: "buffer".to_string(),
            name: stripped,
            value: None,
            focused: true,
            anchor: Some((0, 0)),
            children: Vec::new(),
        }],
    }
}

async fn resolve(registry: &Arc<Registry>, pane: Option<PaneId>) -> Result<Arc<Pane>, Response> {
    if let Some(id) = pane {
        return registry.get(&id).await.ok_or_else(|| {
            Response::err(ErrorBody::new(
                ErrorCode::NoActivePane,
                format!("pane {id} not found"),
                "call list to see live panes",
            ))
        });
    }
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
            "multiple panes; --pane required",
            "pass --pane p<N>",
        ))),
    }
}
