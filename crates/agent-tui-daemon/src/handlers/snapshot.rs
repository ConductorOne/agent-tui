//! `snapshot` command handler.
//!
//! All four modes (`outline` / `cells` / `hybrid` / `adapter`) are wired.
//! Outline content comes from the pane's attached adapter via
//! `Adapter::outline`; an empty or failing adapter falls back to the
//! built-in `generic_outline` heuristic so the agent never gets `null`.

use std::collections::HashMap;
use std::sync::Arc;

use agent_tui_engine::{Cell, EngineSnapshot};
use agent_tui_protocol::request::SnapshotMode;
use agent_tui_protocol::snapshot::CellGridRle;
use agent_tui_protocol::{
    ErrorBody, ErrorCode, Outline, OutlineNode, PaneId, Ref, RefBinding, Response, Selector,
    Snapshot,
};
use base64::Engine as _;

use crate::classifier;
use crate::hash_window::HashWindow;
use crate::pane::{Pane, Registry, resolve_focused};

/// Per-session generation counters keyed by pane id.
///
/// `generation` is *snapshot-driven*: it only ticks the first time a snapshot
/// observes a sequence higher than the last-snapshotted one. Lives on the
/// daemon today; moves onto `Pane` if/when per-pane concurrency demands it.
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

/// Snapshot a pane in the requested mode.
pub async fn run(
    registry: &Arc<Registry>,
    generations: &Arc<GenerationTracker>,
    hashes: &Arc<HashWindow>,
    pane: Option<PaneId>,
    mode: SnapshotMode,
    select: Option<String>,
    all: bool,
) -> Response {
    let pane_arc = match resolve_focused(registry, pane).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    // Compile the selector up-front so a bad expression returns an
    // InvalidArgs error rather than silently producing an empty outline.
    let compiled = match select.as_deref().map(Selector::parse).transpose() {
        Ok(s) => s,
        Err(e) => {
            return Response::err(ErrorBody::new(
                ErrorCode::InvalidArgs,
                format!("selector parse error at byte {}: {}", e.at, e.kind),
                "see docs/addressing-rfc.md §2.2",
            ));
        }
    };

    // §4 of the RFC: `--select` forces an outline. Promote Text/Cells →
    // Hybrid so callers get both the matched outline and the cells/text
    // they asked for.
    let effective_mode = if compiled.is_some() {
        match mode {
            SnapshotMode::Text | SnapshotMode::Cells => SnapshotMode::Hybrid,
            other => other,
        }
    } else {
        mode
    };

    let engine_snap = pane_arc.engine.snapshot();
    let generation = generations
        .observe(&pane_arc.id, engine_snap.sequence)
        .await;
    let adapter = pane_arc.adapter().await;

    let mut snapshot = build_snapshot(
        &pane_arc,
        &adapter,
        &engine_snap,
        generation,
        effective_mode,
    )
    .await;
    if let Some(sel) = compiled.as_ref() {
        snapshot.outline = snapshot.outline.map(|o| filter_outline(o, sel, all));
    }
    // Record (seq, hash) so `wait --hash` can resolve subsequent calls.
    hashes
        .record(&pane_arc.id, snapshot.sequence, snapshot.hash.clone())
        .await;
    match serde_json::to_value(&snapshot) {
        Ok(v) => Response::ok(v),
        Err(e) => Response::err(ErrorBody::new(
            ErrorCode::Internal,
            format!("snapshot serialization failed: {e}"),
            "report a bug",
        )),
    }
}

/// Reduce `outline.nodes` to nodes matching `sel`. Without `all`, keep
/// only the first match (depth-first pre-order). Matched nodes appear at
/// the top level of the filtered outline; their original children are
/// preserved so callers see the matched subtree.
fn filter_outline(outline: Outline, sel: &Selector, all: bool) -> Outline {
    let matches = sel.matches(&outline);
    let nodes: Vec<OutlineNode> = if all {
        matches.into_iter().cloned().collect()
    } else {
        matches.into_iter().next().into_iter().cloned().collect()
    };
    Outline {
        adapter: outline.adapter,
        nodes,
    }
}

async fn build_snapshot(
    pane: &Pane,
    adapter: &Arc<dyn agent_tui_adapter::Adapter>,
    engine_snap: &EngineSnapshot,
    generation: u64,
    mode: SnapshotMode,
) -> Snapshot {
    let hash = engine_snap.canonical_hash();
    let osc = pane.pty.last_osc133_marker();
    let state = classifier::classify_with_osc133(engine_snap, osc);

    // Outline comes from the attached adapter; if it fails or returns an empty
    // node list, fall back to the generic heuristic so we never return nothing
    // to agents.
    let adapter_outline = match adapter.outline(engine_snap).await {
        Ok(o) if !o.nodes.is_empty() => Some(o),
        Ok(_) | Err(_) => None,
    };
    let outline_for_mode = adapter_outline.unwrap_or_else(|| generic_outline(engine_snap));

    let (outline, cells, text) = match mode {
        SnapshotMode::Outline | SnapshotMode::Adapter => (Some(outline_for_mode), None, None),
        SnapshotMode::Cells => (None, Some(rle_grid(engine_snap)), None),
        SnapshotMode::Text => (None, None, Some(grid_to_text(engine_snap))),
        SnapshotMode::Hybrid => (
            Some(outline_for_mode),
            Some(rle_grid(engine_snap)),
            Some(grid_to_text(engine_snap)),
        ),
    };

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
        outline,
        cells,
        text,
        modes: engine_snap.modes.clone(),
        refs,
    }
}

/// Flatten the cell grid into a plain UTF-8 string. Rows joined with
/// `\n`, per-row trailing whitespace trimmed. Empty trailing rows are
/// dropped — agents reading "what does the screen say" rarely want
/// the blank padding.
fn grid_to_text(snap: &EngineSnapshot) -> String {
    let cols = usize::from(snap.grid.cols);
    let rows = usize::from(snap.grid.rows);
    let mut lines: Vec<String> = Vec::with_capacity(rows);
    for row in 0..rows {
        let mut line = String::with_capacity(cols);
        let start = row * cols;
        for col in 0..cols {
            let cell = &snap.grid.cells[start + col];
            // `cell.ch` is a string (may be multi-byte/multi-char for
            // graphemes); just append. Empty `ch` slots are spaces.
            if cell.ch.is_empty() {
                line.push(' ');
            } else {
                line.push_str(&cell.ch);
            }
        }
        // Trim per-row trailing spaces — almost always padding noise.
        let trimmed = line.trim_end().to_string();
        lines.push(trimmed);
    }
    // Drop trailing all-empty rows so a mostly-empty screen doesn't
    // emit a wall of blank lines.
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

/// RLE-compress the cell grid row-by-row and base64-encode each row's JSON.
///
/// Each row encodes as a JSON array of `[count, "ch", width, fg, bg, attrs]`
/// runs — a balance between wire size and tooling friendliness (consumers can
/// decode + parse without a custom binary reader).
fn rle_grid(snap: &EngineSnapshot) -> CellGridRle {
    let cols = usize::from(snap.grid.cols);
    let rows = usize::from(snap.grid.rows);
    let mut rows_b64 = Vec::with_capacity(rows);
    for row in 0..rows {
        let start = row * cols;
        let row_cells = &snap.grid.cells[start..start + cols];
        let runs = encode_row_runs(row_cells);
        let json = serde_json::to_string(&runs).unwrap_or_default();
        rows_b64.push(base64::engine::general_purpose::STANDARD.encode(json.as_bytes()));
    }
    CellGridRle {
        cols: snap.grid.cols,
        rows: snap.grid.rows,
        rows_b64,
        palette: serde_json::Value::Null,
        cursor: snap.grid.cursor,
    }
}

fn encode_row_runs(cells: &[Cell]) -> Vec<serde_json::Value> {
    let mut runs: Vec<serde_json::Value> = Vec::new();
    let mut iter = cells.iter().peekable();
    while let Some(first) = iter.next() {
        let mut count: u32 = 1;
        while let Some(&next) = iter.peek() {
            if next.ch == first.ch
                && next.width == first.width
                && next.fg == first.fg
                && next.bg == first.bg
                && next.attrs == first.attrs
            {
                count += 1;
                iter.next();
            } else {
                break;
            }
        }
        runs.push(serde_json::json!([
            count,
            first.ch,
            first.width,
            first.fg,
            first.bg,
            first.attrs,
        ]));
    }
    runs
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
            ..OutlineNode::default()
        }],
    }
}
