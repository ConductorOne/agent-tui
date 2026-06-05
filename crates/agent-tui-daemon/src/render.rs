//! `snapshot --png` rasterization.
//!
//! Renders the engine's cell grid to an RGB framebuffer — one cell per grid
//! cell at a fixed [`cell_size`] derived from the embedded monospace font —
//! then PNG-encodes it. Glyphs are rasterized with `fontdue` from a vendored
//! `JetBrains Mono` TTF (anti-aliased, alpha-blended over each cell's resolved
//! background). With `--annotate`, ref bounding boxes (anchor → anchor+extent)
//! and `@ref` labels are overlaid; refs without a computable extent fall back
//! to a point marker.
//!
//! Coverage: the embedded font covers Latin/Greek/Cyrillic + box-drawing,
//! block, and common symbols. Glyphs it lacks (emoji, CJK ideographs) render a
//! visible placeholder box rather than panicking — see `render_glyph`.
//!
//! Deliberately dependency-light: `png` (pure-Rust deflate) for encoding and
//! `fontdue` (pure-Rust TrueType rasterizer) for glyphs — no full image-codec
//! stack, no system libraries.

use std::collections::HashMap;
use std::sync::OnceLock;

use agent_tui_engine::EngineSnapshot;
use agent_tui_protocol::{Outline, OutlineNode, Selector};
use fontdue::{Font, Metrics};

/// Embedded monospace font (`JetBrains Mono` Regular, SIL OFL 1.1 — see
/// `assets/OFL.txt`). Vendored so rendering needs no system fonts.
const FONT_BYTES: &[u8] = include_bytes!("../assets/JetBrainsMono-Regular.ttf");

/// Font size in pixels-per-em the grid is rasterized at. Drives the cell size.
const PX: f32 = 16.0;

/// Round a positive pixel metric up to at least 1, as `u32`. Values are small,
/// non-negative font metrics, so the conversion is well-behaved.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn px_to_u32(v: f32) -> u32 {
    v.ceil().max(1.0) as u32
}

/// Default foreground when a cell carries the terminal's default fg.
const DEFAULT_FG: [u8; 3] = [0xd0, 0xd0, 0xd0];
/// Default background when a cell carries the terminal's default bg.
const DEFAULT_BG: [u8; 3] = [0x0a, 0x0a, 0x0a];
/// Overlay color for `--annotate` boxes, markers, and label backgrounds.
pub const OVERLAY: [u8; 3] = [0x14, 0xd4, 0x4a];
/// Label text color (drawn on the [`OVERLAY`] strip).
const LABEL_FG: [u8; 3] = [0x00, 0x00, 0x00];

/// Loaded font + the cell metrics derived from it. Built once.
struct FontCtx {
    font: Font,
    /// Cell width in pixels (monospace advance, rounded up).
    cw: u32,
    /// Cell height in pixels (ascent − descent + line-gap, rounded up).
    ch: u32,
    /// Baseline offset from the cell top, in pixels.
    ascent: i32,
}

fn font_ctx() -> &'static FontCtx {
    static CTX: OnceLock<FontCtx> = OnceLock::new();
    CTX.get_or_init(|| {
        let font = Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default())
            .expect("embedded JetBrains Mono is a valid TTF");
        let lm = font
            .horizontal_line_metrics(PX)
            .expect("font has horizontal line metrics");
        let ch = px_to_u32(lm.ascent - lm.descent + lm.line_gap);
        let ascent = i32::try_from(px_to_u32(lm.ascent)).unwrap_or(0);
        // Monospace: every advance is identical; sample a representative glyph.
        let cw = px_to_u32(font.metrics('M', PX).advance_width);
        FontCtx {
            font,
            cw,
            ch,
            ascent,
        }
    })
}

/// The pixel size of one terminal cell, `(width, height)`, derived from the
/// embedded font. Image dimensions are `cols*cw × rows*ch`.
#[must_use]
pub fn cell_size() -> (u32, u32) {
    let c = font_ctx();
    (c.cw, c.ch)
}

/// A rasterized snapshot: PNG bytes plus the image geometry.
pub struct RenderedPng {
    /// PNG-encoded image bytes, ready to write to disk.
    pub bytes: Vec<u8>,
    /// Image width in pixels (`cols * cell_w`).
    pub width: u32,
    /// Image height in pixels (`rows * cell_h`).
    pub height: u32,
    /// Whether `--annotate` overlays were drawn.
    pub annotated: bool,
}

/// What to overlay, if anything. The `Selector` (when `Some`) restricts the
/// overlay to matching refs; `None` annotates every node in the outline.
pub struct Annotate<'a> {
    /// Outline whose nodes are candidates for overlay.
    pub outline: &'a Outline,
    /// Optional ref filter; `None` annotates all nodes.
    pub selector: Option<&'a Selector>,
}

/// Render `snap` to a PNG, optionally overlaying ref annotations.
///
/// # Errors
/// Returns a message if PNG encoding fails or the grid is empty (0×0).
pub fn render_png(
    snap: &EngineSnapshot,
    annotate: Option<Annotate<'_>>,
) -> Result<RenderedPng, String> {
    let ctx = font_ctx();
    let cols = u32::from(snap.grid.cols);
    let rows = u32::from(snap.grid.rows);
    if cols == 0 || rows == 0 {
        return Err("cannot rasterize an empty (0×0) grid".to_string());
    }
    let width = cols * ctx.cw;
    let height = rows * ctx.ch;

    let mut frame = Frame::new(width, height, DEFAULT_BG);
    let mut glyphs = GlyphCache::new(ctx);
    paint_cells(&mut frame, &mut glyphs, snap, ctx);

    let annotated = if let Some(a) = annotate {
        paint_overlay(&mut frame, &mut glyphs, &a, ctx);
        true
    } else {
        false
    };

    let bytes = encode_png(width, height, &frame.buf)?;
    Ok(RenderedPng {
        bytes,
        width,
        height,
        annotated,
    })
}

/// An RGB framebuffer, row-major, 3 bytes per pixel.
struct Frame {
    width: u32,
    height: u32,
    buf: Vec<u8>,
}

impl Frame {
    fn new(width: u32, height: u32, fill: [u8; 3]) -> Self {
        let px = (width as usize) * (height as usize);
        let mut buf = Vec::with_capacity(px * 3);
        for _ in 0..px {
            buf.extend_from_slice(&fill);
        }
        Self { width, height, buf }
    }

    fn put(&mut self, x: u32, y: u32, rgb: [u8; 3]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = ((y as usize) * (self.width as usize) + (x as usize)) * 3;
        self.buf[i] = rgb[0];
        self.buf[i + 1] = rgb[1];
        self.buf[i + 2] = rgb[2];
    }

    /// Alpha-blend `fg` over the existing pixel with coverage `cov` (0–255).
    fn blend(&mut self, x: u32, y: u32, fg: [u8; 3], cov: u8) {
        if cov == 0 || x >= self.width || y >= self.height {
            return;
        }
        let i = ((y as usize) * (self.width as usize) + (x as usize)) * 3;
        let a = u16::from(cov);
        let inv = 255 - a;
        for (dst, &f) in self.buf[i..i + 3].iter_mut().zip(fg.iter()) {
            let blended = (u16::from(f) * a + u16::from(*dst) * inv) / 255;
            *dst = u8::try_from(blended).unwrap_or(255);
        }
    }

    fn fill_rect(&mut self, x0: u32, y0: u32, w: u32, h: u32, rgb: [u8; 3]) {
        for y in y0..y0.saturating_add(h) {
            for x in x0..x0.saturating_add(w) {
                self.put(x, y, rgb);
            }
        }
    }

    /// Draw a 1px hollow rectangle outline, clamped to the frame.
    fn draw_rect_outline(&mut self, x0: u32, y0: u32, w: u32, h: u32, rgb: [u8; 3]) {
        if w == 0 || h == 0 {
            return;
        }
        let x1 = x0.saturating_add(w - 1).min(self.width.saturating_sub(1));
        let y1 = y0.saturating_add(h - 1).min(self.height.saturating_sub(1));
        for x in x0..=x1 {
            self.put(x, y0, rgb);
            self.put(x, y1, rgb);
        }
        for y in y0..=y1 {
            self.put(x0, y, rgb);
            self.put(x1, y, rgb);
        }
    }
}

/// Per-render glyph cache: rasterize each `char` once and reuse its coverage
/// bitmap (color is applied at blend time, so it's color-independent).
struct GlyphCache<'a> {
    ctx: &'a FontCtx,
    cache: HashMap<char, Glyph>,
}

/// A rasterized glyph: its `fontdue` metrics + coverage bitmap, and whether
/// the font actually had the glyph (`false` → a placeholder is drawn instead).
struct Glyph {
    metrics: Metrics,
    bitmap: Vec<u8>,
    present: bool,
}

impl<'a> GlyphCache<'a> {
    fn new(ctx: &'a FontCtx) -> Self {
        Self {
            ctx,
            cache: HashMap::new(),
        }
    }

    fn get(&mut self, ch: char) -> &Glyph {
        let ctx = self.ctx;
        self.cache.entry(ch).or_insert_with(|| {
            let present = ctx.font.lookup_glyph_index(ch) != 0;
            let (metrics, bitmap) = ctx.font.rasterize(ch, PX);
            Glyph {
                metrics,
                bitmap,
                present,
            }
        })
    }
}

/// A cell's pixel rectangle origin + width that a glyph renders into.
#[derive(Clone, Copy)]
struct CellBox {
    x: u32,
    y: u32,
    w: u32,
}

/// Draw `ch` in color `fg` into `cell`. Missing glyphs render a hollow
/// placeholder box so the gap is visible (never panic).
fn render_glyph(
    frame: &mut Frame,
    glyphs: &mut GlyphCache<'_>,
    cell: CellBox,
    ch: char,
    fg: [u8; 3],
) {
    let ascent = glyphs.ctx.ascent;
    let cell_h = glyphs.ctx.ch;
    let g = glyphs.get(ch);
    if !g.present {
        // .notdef fallback: a hollow box inset in the cell.
        let inset = (cell_h / 6).max(1);
        frame.draw_rect_outline(
            cell.x + 1,
            cell.y + inset,
            cell.w.saturating_sub(2),
            cell_h.saturating_sub(inset * 2),
            fg,
        );
        return;
    }
    let baseline = i32::try_from(cell.y).unwrap_or(0) + ascent;
    let gx0 = i32::try_from(cell.x).unwrap_or(0) + g.metrics.xmin;
    let gy0 = baseline - i32::try_from(g.metrics.height).unwrap_or(0) - g.metrics.ymin;
    for row in 0..g.metrics.height {
        for col in 0..g.metrics.width {
            let cov = g.bitmap[row * g.metrics.width + col];
            if cov == 0 {
                continue;
            }
            let px = gx0 + i32::try_from(col).unwrap_or(0);
            let py = gy0 + i32::try_from(row).unwrap_or(0);
            let (Ok(px), Ok(py)) = (u32::try_from(px), u32::try_from(py)) else {
                continue;
            };
            frame.blend(px, py, fg, cov);
        }
    }
}

/// Paint every grid cell's background + glyph into the frame.
fn paint_cells(
    frame: &mut Frame,
    glyphs: &mut GlyphCache<'_>,
    snap: &EngineSnapshot,
    ctx: &FontCtx,
) {
    let cols = usize::from(snap.grid.cols);
    let rows = usize::from(snap.grid.rows);
    for row in 0..rows {
        let y = u32::try_from(row).unwrap_or(0) * ctx.ch;
        let mut col = 0usize;
        while col < cols {
            let cell = &snap.grid.cells[row * cols + col];
            // A width-2 (CJK/wide) glyph advances two cells; width-0 is a
            // continuation spacer we cover and skip.
            let span = if cell.width == 2 { 2u32 } else { 1u32 };
            let x = u32::try_from(col).unwrap_or(0) * ctx.cw;
            let mut fg = resolve_rgb(cell.fg, DEFAULT_FG);
            let mut bg = resolve_rgb(cell.bg, DEFAULT_BG);
            // attrs bit 3 = inverse (see `pack_attrs` in the alacritty engine).
            if cell.attrs & (1 << 3) != 0 {
                std::mem::swap(&mut fg, &mut bg);
            }
            frame.fill_rect(x, y, span * ctx.cw, ctx.ch, bg);
            if let Some(c) = cell.ch.chars().next() {
                if c != ' ' && !c.is_control() {
                    let cellbox = CellBox {
                        x,
                        y,
                        w: span * ctx.cw,
                    };
                    render_glyph(frame, glyphs, cellbox, c, fg);
                }
            }
            col += span as usize;
        }
    }
}

/// Overlay ref boxes/markers + labels for the selected nodes.
fn paint_overlay(frame: &mut Frame, glyphs: &mut GlyphCache<'_>, a: &Annotate<'_>, ctx: &FontCtx) {
    let nodes: Vec<&OutlineNode> = if let Some(sel) = a.selector {
        sel.matches(a.outline)
    } else {
        let mut acc = Vec::new();
        for n in &a.outline.nodes {
            collect(n, &mut acc);
        }
        acc
    };
    for node in nodes {
        let Some((arow, acol)) = node.anchor else {
            continue;
        };
        let x = u32::from(acol) * ctx.cw;
        let y = u32::from(arow) * ctx.ch;
        match node.extent {
            Some((ecols, erows)) if ecols > 0 && erows > 0 => {
                frame.draw_rect_outline(
                    x,
                    y,
                    u32::from(ecols) * ctx.cw,
                    u32::from(erows) * ctx.ch,
                    OVERLAY,
                );
            }
            _ => {
                // No computable span → point marker at the anchor cell.
                frame.fill_rect(x, y, ctx.cw, ctx.ch, OVERLAY);
            }
        }
        draw_label(frame, glyphs, ctx, x, y, &node.r#ref);
    }
}

/// Draw a ref label: an [`OVERLAY`] strip with the `@ref` text in [`LABEL_FG`],
/// clamped so a label anchored at an edge stays visible.
fn draw_label(
    frame: &mut Frame,
    glyphs: &mut GlyphCache<'_>,
    ctx: &FontCtx,
    x: u32,
    y: u32,
    text: &str,
) {
    let n = u32::try_from(text.chars().count()).unwrap_or(0);
    if n == 0 {
        return;
    }
    let w = (n * ctx.cw).min(frame.width);
    let lx = x.min(frame.width.saturating_sub(w));
    let ly = y.min(frame.height.saturating_sub(ctx.ch.min(frame.height)));
    frame.fill_rect(lx, ly, w, ctx.ch, OVERLAY);
    for (i, c) in text.chars().enumerate() {
        let cx = lx + u32::try_from(i).unwrap_or(0) * ctx.cw;
        if !c.is_control() {
            let cellbox = CellBox {
                x: cx,
                y: ly,
                w: ctx.cw,
            };
            render_glyph(frame, glyphs, cellbox, c, LABEL_FG);
        }
    }
}

/// Depth-first collect a node and all its descendants.
fn collect<'a>(node: &'a OutlineNode, acc: &mut Vec<&'a OutlineNode>) {
    acc.push(node);
    for child in &node.children {
        collect(child, acc);
    }
}

/// Resolve a packed color (see `encode_color` in the alacritty engine) to RGB.
/// RGB-bit colors decode directly; `0..=255` index the xterm-256 palette;
/// anything else (named defaults like Foreground/Background) uses `default`.
fn resolve_rgb(packed: u32, default: [u8; 3]) -> [u8; 3] {
    if packed & 0x0100_0000 != 0 {
        [
            u8::try_from((packed >> 16) & 0xff).unwrap_or(0),
            u8::try_from((packed >> 8) & 0xff).unwrap_or(0),
            u8::try_from(packed & 0xff).unwrap_or(0),
        ]
    } else if let Ok(idx) = u8::try_from(packed) {
        palette_rgb(idx)
    } else {
        default
    }
}

/// xterm-256 palette → RGB: 16 ANSI base colors, a 6×6×6 cube, then a
/// 24-step grayscale ramp.
fn palette_rgb(idx: u8) -> [u8; 3] {
    const ANSI: [[u8; 3]; 16] = [
        [0x00, 0x00, 0x00],
        [0x80, 0x00, 0x00],
        [0x00, 0x80, 0x00],
        [0x80, 0x80, 0x00],
        [0x00, 0x00, 0x80],
        [0x80, 0x00, 0x80],
        [0x00, 0x80, 0x80],
        [0xc0, 0xc0, 0xc0],
        [0x80, 0x80, 0x80],
        [0xff, 0x00, 0x00],
        [0x00, 0xff, 0x00],
        [0xff, 0xff, 0x00],
        [0x00, 0x00, 0xff],
        [0xff, 0x00, 0xff],
        [0x00, 0xff, 0xff],
        [0xff, 0xff, 0xff],
    ];
    match idx {
        0..=15 => ANSI[idx as usize],
        16..=231 => {
            let i = idx - 16;
            let r = i / 36;
            let g = (i / 6) % 6;
            let b = i % 6;
            [cube(r), cube(g), cube(b)]
        }
        232..=255 => {
            let level = 8 + 10 * (idx - 232);
            [level, level, level]
        }
    }
}

/// Map a 0..=5 color-cube coordinate to an 8-bit channel value.
fn cube(c: u8) -> u8 {
    if c == 0 { 0 } else { 55 + c * 40 }
}

/// PNG-encode an RGB framebuffer (`width*height*3` bytes).
fn encode_png(width: u32, height: u32, rgb: &[u8]) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| format!("png header: {e}"))?;
        writer
            .write_image_data(rgb)
            .map_err(|e| format!("png data: {e}"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_tui_engine::{Cell, CellGrid, ModeFlags};

    fn snap_row(cells: &[(&str, u8, u32, u32)]) -> EngineSnapshot {
        let grid_cells = cells
            .iter()
            .map(|&(ch, width, fg, bg)| Cell {
                ch: ch.to_string(),
                width,
                fg,
                bg,
                attrs: 0,
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

    #[test]
    fn cell_size_is_sane() {
        let (cw, ch) = cell_size();
        assert!((4..=32).contains(&cw), "cw {cw}");
        assert!((8..=48).contains(&ch), "ch {ch}");
    }

    #[test]
    fn renders_png_with_expected_dims_and_a_glyph() {
        let (cw, ch) = cell_size();
        let snap = snap_row(&[("h", 1, 256, 257), ("i", 1, 1, 257)]);
        let r = render_png(&snap, None).expect("render");
        assert_eq!(r.width, 2 * cw);
        assert_eq!(r.height, ch);
        assert!(!r.annotated);
        assert_eq!(&r.bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn glyphs_actually_paint_pixels() {
        // A row of text must produce non-background pixels (a real glyph),
        // proving we draw the font, not just a blank canvas.
        let glyph = render_decoded(&snap_row(&[("X", 1, 256, 257)]));
        assert!(
            glyph.iter().any(|&b| b != DEFAULT_BG[0]),
            "expected rendered glyph pixels, got an all-background image"
        );
    }

    #[test]
    fn missing_glyph_draws_placeholder_not_panic() {
        // An emoji the embedded font lacks must render a placeholder (visible
        // pixels) without panicking.
        let px = render_decoded(&snap_row(&[("\u{1F600}", 2, 256, 257)]));
        assert!(px.iter().any(|&b| b != DEFAULT_BG[0]), "placeholder drawn");
    }

    #[test]
    fn annotate_changes_output() {
        let snap = snap_row(&[("h", 1, 256, 257), ("i", 1, 1, 257)]);
        let outline = Outline {
            adapter: "generic".into(),
            nodes: vec![OutlineNode {
                r#ref: "@e1".into(),
                role: "buffer".into(),
                anchor: Some((0, 0)),
                extent: Some((2, 1)),
                ..OutlineNode::default()
            }],
        };
        let plain = render_png(&snap, None).expect("plain");
        let annotated = render_png(
            &snap,
            Some(Annotate {
                outline: &outline,
                selector: None,
            }),
        )
        .expect("annotated");
        assert!(annotated.annotated);
        assert_ne!(plain.bytes, annotated.bytes);
    }

    #[test]
    fn palette_cube_and_grayscale() {
        assert_eq!(palette_rgb(0), [0, 0, 0]);
        assert_eq!(palette_rgb(15), [0xff, 0xff, 0xff]);
        assert_eq!(palette_rgb(16), [0, 0, 0]);
        assert_eq!(palette_rgb(231), [0xff, 0xff, 0xff]);
        assert_eq!(palette_rgb(232), [8, 8, 8]);
    }

    /// Render to PNG then decode back to raw RGB for pixel assertions.
    fn render_decoded(snap: &EngineSnapshot) -> Vec<u8> {
        let r = render_png(snap, None).expect("render");
        let decoder = png::Decoder::new(std::io::Cursor::new(r.bytes));
        let mut reader = decoder.read_info().expect("read_info");
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).expect("next_frame");
        buf.truncate(info.buffer_size());
        buf
    }
}
