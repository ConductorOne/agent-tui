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
    Snapshot, Warning, format_selector_parse_error, outline_all_refs,
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

/// What to render for a snapshot. Bundled so `run` keeps a sane arity.
pub struct SnapshotParams {
    /// Rendering mode (outline / text / cells / adapter / hybrid).
    pub mode: SnapshotMode,
    /// Optional outline selector (RFC §2.2).
    pub select: Option<String>,
    /// With `select`, return every match rather than just the first.
    pub all: bool,
    /// With text/hybrid, reconstruct per-cell SGR instead of stripping.
    pub keep_color: bool,
}

/// Snapshot a pane in the requested mode.
pub async fn run(
    registry: &Arc<Registry>,
    generations: &Arc<GenerationTracker>,
    hashes: &Arc<HashWindow>,
    pane: Option<PaneId>,
    params: SnapshotParams,
) -> Response {
    let SnapshotParams {
        mode,
        select,
        all,
        keep_color,
    } = params;
    let pane_arc = match resolve_focused(registry, pane).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    // Compile the selector up-front so a bad expression returns an
    // InvalidArgs error rather than silently producing an empty outline.
    let compiled = match select.as_deref().map(Selector::parse).transpose() {
        Ok(s) => s,
        Err(e) => {
            let raw = select.as_deref().unwrap_or("");
            return Response::err(ErrorBody::new(
                ErrorCode::InvalidArgs,
                format_selector_parse_error(raw, &e),
                "see docs/addressing-rfc.md §2.2 or `agent-tui skills get addressing`",
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
        keep_color,
    )
    .await;
    // Carry forward `select` so we can attach a "no-match" warning
    // with the available refs below.
    let mut select_miss_refs: Option<Vec<String>> = None;
    if let Some(sel) = compiled.as_ref() {
        if let Some(orig) = snapshot.outline.clone() {
            let matches = sel.matches(&orig);
            if matches.is_empty() {
                select_miss_refs = Some(outline_all_refs(&orig, 20));
            }
            snapshot.outline = Some(filter_outline(orig, sel, all));
        }
    }
    // Record (seq, hash) so `wait --hash` can resolve subsequent calls.
    hashes
        .record(&pane_arc.id, snapshot.sequence, snapshot.hash.clone())
        .await;
    match serde_json::to_value(&snapshot) {
        Ok(v) => {
            let mut resp = Response::ok(v);
            if let Some(refs) = select_miss_refs {
                resp = resp.with_warning(Warning {
                    code: "selector_no_match".into(),
                    message: if refs.is_empty() {
                        "selector matched no node; outline is empty".into()
                    } else {
                        format!(
                            "selector matched no node. available refs: {}",
                            refs.join(", ")
                        )
                    },
                });
            }
            resp
        }
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
    keep_color: bool,
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
        SnapshotMode::Text => (None, None, Some(grid_to_text(engine_snap, keep_color))),
        SnapshotMode::Hybrid => (
            Some(outline_for_mode),
            Some(rle_grid(engine_snap)),
            Some(grid_to_text(engine_snap, keep_color)),
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

/// Flatten the cell grid into a string. Rows joined with `\n`, trailing
/// padding trimmed, empty trailing rows dropped — agents reading "what
/// does the screen say" rarely want the blank padding.
///
/// With `keep_color`, a styled run is opened with a self-contained SGR
/// escape (reconstructed from the cell's fg/bg/attrs) and held until the
/// style changes — consecutive identical-style cells share one escape —
/// then the row is reset (`ESC[0m`) at its last styled cell. Without it,
/// the output is plain UTF-8 with no escapes.
fn grid_to_text(snap: &EngineSnapshot, keep_color: bool) -> String {
    let cols = usize::from(snap.grid.cols);
    let rows = usize::from(snap.grid.rows);
    let mut lines: Vec<String> = Vec::with_capacity(rows);
    for row in 0..rows {
        let start = row * cols;
        let row_cells = &snap.grid.cells[start..start + cols];
        // Rightmost column worth emitting: any non-blank glyph, or (in
        // color mode) any styled cell — a colored space still carries a
        // background we must not trim away.
        let last = row_cells.iter().rposition(|c| {
            let blank = c.ch.is_empty() || c.ch == " ";
            !blank || (keep_color && cell_sgr(c.fg, c.bg, c.attrs).is_some())
        });
        let Some(last) = last else {
            lines.push(String::new());
            continue;
        };
        let mut line = String::with_capacity(last + 1);
        // The currently-open style (`None` = default / nothing open).
        let mut cur: Option<String> = None;
        for cell in &row_cells[..=last] {
            if keep_color {
                let sgr = cell_sgr(cell.fg, cell.bg, cell.attrs);
                if sgr != cur {
                    match &sgr {
                        Some(open) => line.push_str(open),
                        None => line.push_str("\x1b[0m"),
                    }
                    cur = sgr;
                }
            }
            if cell.ch.is_empty() {
                line.push(' ');
            } else {
                line.push_str(&cell.ch);
            }
        }
        if cur.is_some() {
            line.push_str("\x1b[0m");
        }
        lines.push(line);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

/// Reconstruct the SGR escape for a cell's style, or `None` if the cell is
/// fully default (so default cells emit no escape at all). Leads with `0`
/// (reset) so each run is self-contained — no need to diff against the
/// previous cell. The attribute bit layout mirrors `pack_attrs` in the
/// alacritty engine: 0=bold 1=italic 2=underline 3=inverse 4=strikeout.
fn cell_sgr(fg: u32, bg: u32, attrs: u8) -> Option<String> {
    let mut params: Vec<String> = Vec::new();
    for (bit, code) in [(0u8, "1"), (1, "3"), (2, "4"), (3, "7"), (4, "9")] {
        if attrs & (1 << bit) != 0 {
            params.push(code.to_string());
        }
    }
    if let Some(c) = color_param(fg, 38) {
        params.push(c);
    }
    if let Some(c) = color_param(bg, 48) {
        params.push(c);
    }
    if params.is_empty() {
        None
    } else {
        Some(format!("\x1b[0;{}m", params.join(";")))
    }
}

/// Map a packed color (see `encode_color` in the alacritty engine) to an
/// SGR color parameter under `base` (38 = foreground, 48 = background).
/// Returns `None` for the terminal default, encoded as a `NamedColor`
/// special (value >= 256 without the RGB bit).
fn color_param(color: u32, base: u32) -> Option<String> {
    if color & 0x0100_0000 != 0 {
        let r = (color >> 16) & 0xff;
        let g = (color >> 8) & 0xff;
        let b = color & 0xff;
        Some(format!("{base};2;{r};{g};{b}"))
    } else if color <= 255 {
        Some(format!("{base};5;{color}"))
    } else {
        None
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use agent_tui_engine::{CellGrid, ModeFlags};

    /// Build a one-row snapshot from `(ch, fg, bg, attrs)` tuples.
    fn row(cells: &[(&str, u32, u32, u8)]) -> EngineSnapshot {
        let grid_cells = cells
            .iter()
            .map(|&(ch, fg, bg, attrs)| Cell {
                ch: ch.to_string(),
                width: 1,
                fg,
                bg,
                attrs,
            })
            .collect::<Vec<_>>();
        EngineSnapshot {
            grid: CellGrid {
                cols: u16::try_from(cells.len()).unwrap(),
                rows: 1,
                cells: grid_cells,
                cursor: (0, 0),
            },
            modes: ModeFlags::default(),
            sequence: 0,
        }
    }

    // Default fg/bg in alacritty's encoding: NamedColor::Foreground/Background
    // are >= 256, so a fully-default cell carries no color.
    const DEF: u32 = 256;

    #[test]
    fn plain_mode_emits_no_escapes() {
        let snap = row(&[("h", 1, DEF, 0), ("i", DEF, DEF, 0)]);
        let out = grid_to_text(&snap, false);
        assert_eq!(out, "hi");
        assert!(!out.contains('\x1b'));
    }

    #[test]
    fn keep_color_reconstructs_named_fg_and_resets() {
        // Red ("Named(Red)" == palette index 1) then a default cell.
        let snap = row(&[("R", 1, DEF, 0), ("x", DEF, DEF, 0)]);
        let out = grid_to_text(&snap, true);
        assert_eq!(out, "\x1b[0;38;5;1mR\x1b[0mx");
    }

    #[test]
    fn keep_color_coalesces_a_same_style_run() {
        // Three red cells share one open escape and one reset, not three.
        let snap = row(&[("R", 1, DEF, 0), ("E", 1, DEF, 0), ("D", 1, DEF, 0)]);
        let out = grid_to_text(&snap, true);
        assert_eq!(out, "\x1b[0;38;5;1mRED\x1b[0m");
    }

    #[test]
    fn keep_color_reopens_on_style_change() {
        // Red then green: the green run reopens (its `0;` resets red first).
        let snap = row(&[("R", 1, DEF, 0), ("G", 2, DEF, 0)]);
        let out = grid_to_text(&snap, true);
        assert_eq!(out, "\x1b[0;38;5;1mR\x1b[0;38;5;2mG\x1b[0m");
    }

    #[test]
    fn keep_color_rgb_truecolor() {
        // Spec(0x10_20_30) → RGB bit set.
        let rgb = 0x0100_0000 | 0x0010_2030;
        let snap = row(&[("A", rgb, DEF, 0)]);
        let out = grid_to_text(&snap, true);
        assert_eq!(out, "\x1b[0;38;2;16;32;48mA\x1b[0m");
    }

    #[test]
    fn keep_color_attrs_and_bg() {
        // bold (bit 0) + underline (bit 2) = 0b101 = 5, bg = palette 4.
        let snap = row(&[("B", DEF, 4, 0b101)]);
        let out = grid_to_text(&snap, true);
        assert_eq!(out, "\x1b[0;1;4;48;5;4mB\x1b[0m");
    }

    #[test]
    fn keep_color_preserves_trailing_colored_space_that_plain_trims() {
        // A trailing space carrying a background color must survive in
        // color mode but be trimmed as padding in plain mode.
        let snap = row(&[("x", DEF, DEF, 0), (" ", DEF, 2, 0)]);
        assert_eq!(grid_to_text(&snap, false), "x");
        assert_eq!(grid_to_text(&snap, true), "x\x1b[0;48;5;2m \x1b[0m");
    }

    #[test]
    fn default_only_row_trims_to_empty() {
        let snap = row(&[(" ", DEF, DEF, 0), (" ", DEF, DEF, 0)]);
        assert_eq!(grid_to_text(&snap, true), "");
        assert_eq!(grid_to_text(&snap, false), "");
    }
}
