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
//! Cell attributes (`Cell.attrs`) are honored: bold and italic select the
//! matching embedded face, dim renders at half coverage, and underline /
//! strikethrough are drawn as baseline-relative rules. Inverse swaps fg/bg.
//!
//! Window chrome (a marketing-grade frame: padding, rounded corners, a title
//! bar with traffic-light dots, a drop shadow, and a brand-dark backdrop) is an
//! opt-in post-process pass — see [`chrome`]. The bare grid render is unchanged
//! when chrome is off.
//!
//! Deliberately dependency-light: `png` (pure-Rust deflate) for encoding and
//! `fontdue` (pure-Rust TrueType rasterizer) for glyphs — no full image-codec
//! stack, no system libraries. The chrome pass adds `tiny-skia` (BSD-3-Clause,
//! pure-Rust 2D raster).

mod chrome;
pub use chrome::ChromeOptions;

use std::collections::HashMap;
use std::sync::OnceLock;

use agent_tui_engine::EngineSnapshot;
use agent_tui_protocol::{Outline, OutlineNode, Selector};
use fontdue::{Font, Metrics};

/// Embedded monospace fonts (`JetBrains Mono`, SIL OFL 1.1 — see `assets/OFL.txt`).
/// Vendored so rendering needs no system fonts. The Regular face drives cell
/// metrics; the Bold/Italic/BoldItalic faces are metric-compatible (identical
/// advance + line metrics) and selected per-cell from `Cell.attrs`.
const FONT_REGULAR: &[u8] = include_bytes!("../assets/JetBrainsMono-Regular.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../assets/JetBrainsMono-Bold.ttf");
const FONT_ITALIC: &[u8] = include_bytes!("../assets/JetBrainsMono-Italic.ttf");
const FONT_BOLD_ITALIC: &[u8] = include_bytes!("../assets/JetBrainsMono-BoldItalic.ttf");

/// Font size in pixels-per-em the grid is rasterized at. Drives the cell size.
const PX: f32 = 16.0;

/// Round a positive pixel metric up to at least 1, as `u32`. Values are small,
/// non-negative font metrics, so the conversion is well-behaved.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn px_to_u32(v: f32) -> u32 {
    v.ceil().max(1.0) as u32
}

/// Default foreground when a cell carries the terminal's default fg.
/// (Dracula `foreground` — soft off-white, easy on a dark backdrop.)
const DEFAULT_FG: [u8; 3] = [0xf8, 0xf8, 0xf2];
/// Default background when a cell carries the terminal's default bg.
/// (Dracula `background` — deep desaturated indigo, not flat black.)
const DEFAULT_BG: [u8; 3] = [0x28, 0x2a, 0x36];
/// Overlay stroke color for `--annotate` boxes/markers (agent-browser blue).
pub const OVERLAY: [u8; 3] = [0x3b, 0x82, 0xf6];
/// Badge fill color for `--annotate` numbered badges (a brighter accent).
const BADGE_BG: [u8; 3] = [0x3b, 0x82, 0xf6];
/// Badge / label text color (drawn on the [`BADGE_BG`] fill).
const LABEL_FG: [u8; 3] = [0xff, 0xff, 0xff];

// `Cell.attrs` bit layout (see `pack_attrs` in the alacritty engine):
//   bit 0 = bold, 1 = italic, 2 = underline, 3 = inverse, 4 = strikeout,
//   bit 5 = dim/faint.
const ATTR_BOLD: u8 = 1 << 0;
const ATTR_ITALIC: u8 = 1 << 1;
const ATTR_UNDERLINE: u8 = 1 << 2;
const ATTR_INVERSE: u8 = 1 << 3;
const ATTR_STRIKEOUT: u8 = 1 << 4;
const ATTR_DIM: u8 = 1 << 5;

/// The four embedded faces + the cell metrics derived from the Regular face.
/// All faces are metric-compatible, so cell size is face-independent. Built once.
struct FontCtx {
    /// `[regular, bold, italic, bold-italic]`, indexed by [`face_index`].
    faces: [Font; 4],
    /// Cell width in pixels (monospace advance, rounded up).
    cw: u32,
    /// Cell height in pixels (ascent − descent + line-gap, rounded up).
    ch: u32,
    /// Baseline offset from the cell top, in pixels.
    ascent: i32,
}

/// Map `(bold, italic)` to an index into [`FontCtx::faces`].
fn face_index(bold: bool, italic: bool) -> usize {
    usize::from(bold) | (usize::from(italic) << 1)
}

fn font_ctx() -> &'static FontCtx {
    static CTX: OnceLock<FontCtx> = OnceLock::new();
    CTX.get_or_init(|| {
        let load = |bytes: &[u8]| {
            Font::from_bytes(bytes, fontdue::FontSettings::default())
                .expect("embedded JetBrains Mono is a valid TTF")
        };
        let faces = [
            load(FONT_REGULAR),
            load(FONT_BOLD),
            load(FONT_ITALIC),
            load(FONT_BOLD_ITALIC),
        ];
        // Cell metrics derive from the Regular face; all faces share them.
        let regular = &faces[0];
        let lm = regular
            .horizontal_line_metrics(PX)
            .expect("font has horizontal line metrics");
        let ch = px_to_u32(lm.ascent - lm.descent + lm.line_gap);
        let ascent = i32::try_from(px_to_u32(lm.ascent)).unwrap_or(0);
        // Monospace: every advance is identical; sample a representative glyph.
        let cw = px_to_u32(regular.metrics('M', PX).advance_width);
        FontCtx {
            faces,
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

/// Render `snap` to a PNG, optionally overlaying ref annotations and/or
/// compositing a marketing-grade window frame.
///
/// With `chrome = None` the output is the bare cell grid (`cols*cw × rows*ch`),
/// unchanged in geometry from before this option existed. With `chrome =
/// Some(..)` the painted grid is composited into a framed image (padding,
/// rounded window body, title bar + traffic lights, drop shadow, brand-dark
/// backdrop) — larger dimensions, ship-nice presentation.
///
/// # Errors
/// Returns a message if PNG encoding fails or the grid is empty (0×0).
pub fn render_png(
    snap: &EngineSnapshot,
    annotate: Option<Annotate<'_>>,
    chrome: Option<&ChromeOptions>,
) -> Result<RenderedPng, String> {
    let ctx = font_ctx();
    let cols = u32::from(snap.grid.cols);
    let rows = u32::from(snap.grid.rows);
    if cols == 0 || rows == 0 {
        return Err("cannot rasterize an empty (0×0) grid".to_string());
    }
    let grid_w = cols * ctx.cw;
    let grid_h = rows * ctx.ch;

    let mut frame = Frame::new(grid_w, grid_h, DEFAULT_BG);
    let mut glyphs = GlyphCache::new(ctx);
    paint_cells(&mut frame, &mut glyphs, snap, ctx);

    let annotated = if let Some(a) = annotate {
        paint_overlay(&mut frame, &mut glyphs, &a, ctx);
        true
    } else {
        false
    };

    // Optional window-chrome post-process pass. Operates on the painted RGB
    // grid; the cell renderer above is untouched.
    if let Some(opts) = chrome {
        let title = opts
            .title
            .clone()
            .or_else(|| snap.title.clone())
            .unwrap_or_default();
        let _ = opts;
        let framed = chrome::composite(&frame.buf, grid_w, grid_h, &title);
        let bytes = encode_png(framed.width, framed.height, &framed.buf)?;
        return Ok(RenderedPng {
            bytes,
            width: framed.width,
            height: framed.height,
            annotated,
        });
    }

    let bytes = encode_png(grid_w, grid_h, &frame.buf)?;
    Ok(RenderedPng {
        bytes,
        width: grid_w,
        height: grid_h,
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

    /// Alpha-blend a solid `rgb` rectangle over the frame at coverage `cov`.
    fn blend_rect(&mut self, x0: u32, y0: u32, w: u32, h: u32, rgb: [u8; 3], cov: u8) {
        for y in y0..y0.saturating_add(h) {
            for x in x0..x0.saturating_add(w) {
                self.blend(x, y, rgb, cov);
            }
        }
    }

    /// Fill a rounded rectangle: the body minus quarter-circle corners of
    /// radius `r`. Used for `--annotate` numbered badges.
    fn fill_round_rect(&mut self, x0: u32, y0: u32, w: u32, h: u32, r: u32, rgb: [u8; 3]) {
        if w == 0 || h == 0 {
            return;
        }
        let r = r.min(w / 2).min(h / 2);
        let r2 = i64::from(r) * i64::from(r);
        for y in 0..h {
            for x in 0..w {
                // Distance from the nearest corner center; outside the radius
                // in a corner quadrant → skip (rounded).
                let in_corner = |cx: u32, cy: u32| -> bool {
                    let dx = i64::from(x) - i64::from(cx);
                    let dy = i64::from(y) - i64::from(cy);
                    dx * dx + dy * dy > r2
                };
                let skip = (x < r && y < r && in_corner(r, r))
                    || (x >= w - r && y < r && in_corner(w - r - 1, r))
                    || (x < r && y >= h - r && in_corner(r, h - r - 1))
                    || (x >= w - r && y >= h - r && in_corner(w - r - 1, h - r - 1));
                if !skip {
                    self.put(x0 + x, y0 + y, rgb);
                }
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

/// Per-render glyph cache: rasterize each `(char, bold, italic)` once and reuse
/// its coverage bitmap (color is applied at blend time, so it's
/// color-independent). The face index disambiguates the same char rendered in
/// different weights/slants.
struct GlyphCache<'a> {
    ctx: &'a FontCtx,
    cache: HashMap<(char, usize), Glyph>,
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

    fn get(&mut self, ch: char, face: usize) -> &Glyph {
        let ctx = self.ctx;
        self.cache.entry((ch, face)).or_insert_with(|| {
            let font = &ctx.faces[face];
            let present = font.lookup_glyph_index(ch) != 0;
            let (metrics, bitmap) = font.rasterize(ch, PX);
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

/// Per-glyph rendering style resolved from `Cell.attrs`. Mirrors the terminal
/// SGR attribute bits one-to-one, so a bool per attribute is the natural shape.
#[derive(Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)]
struct GlyphStyle {
    bold: bool,
    italic: bool,
    dim: bool,
    underline: bool,
    strikeout: bool,
}

impl GlyphStyle {
    /// Coverage scale applied to every glyph pixel. Dim (faint) renders at
    /// half coverage, matching agg's `faint = 0.5 alpha`.
    fn coverage_scale(self) -> u16 {
        if self.dim { 128 } else { 255 }
    }
}

/// Draw `ch` in color `fg` into `cell`, honoring `style` (bold/italic face
/// selection, dim coverage, underline, strikethrough). Missing glyphs render a
/// hollow placeholder box so the gap is visible (never panic).
#[allow(clippy::similar_names)] // `gx0`/`gy0` are the glyph's x/y origin — clear in context
fn render_glyph(
    frame: &mut Frame,
    glyphs: &mut GlyphCache<'_>,
    cell: CellBox,
    ch: char,
    fg: [u8; 3],
    style: GlyphStyle,
) {
    let ascent = glyphs.ctx.ascent;
    let cell_h = glyphs.ctx.ch;
    let scale = style.coverage_scale();
    let face = face_index(style.bold, style.italic);
    let g = glyphs.get(ch, face);
    if g.present {
        let baseline = i32::try_from(cell.y).unwrap_or(0) + ascent;
        let gx0 = i32::try_from(cell.x).unwrap_or(0) + g.metrics.xmin;
        let gy0 = baseline - i32::try_from(g.metrics.height).unwrap_or(0) - g.metrics.ymin;
        for row in 0..g.metrics.height {
            for col in 0..g.metrics.width {
                let cov = g.bitmap[row * g.metrics.width + col];
                if cov == 0 {
                    continue;
                }
                let cov = u8::try_from(u16::from(cov) * scale / 255).unwrap_or(cov);
                let px = gx0 + i32::try_from(col).unwrap_or(0);
                let py = gy0 + i32::try_from(row).unwrap_or(0);
                let (Ok(px), Ok(py)) = (u32::try_from(px), u32::try_from(py)) else {
                    continue;
                };
                frame.blend(px, py, fg, cov);
            }
        }
    } else {
        // .notdef fallback: a hollow box inset in the cell.
        let inset = (cell_h / 6).max(1);
        frame.draw_rect_outline(
            cell.x + 1,
            cell.y + inset,
            cell.w.saturating_sub(2),
            cell_h.saturating_sub(inset * 2),
            fg,
        );
    }
    // Decorations span the full cell width and sit at baseline-relative rows.
    if style.underline {
        let y = i32::try_from(cell.y).unwrap_or(0) + ascent + 1;
        draw_hline(frame, cell.x, cell.w, y, fg, scale);
    }
    if style.strikeout {
        let y = i32::try_from(cell.y).unwrap_or(0) + ascent - ascent / 3;
        draw_hline(frame, cell.x, cell.w, y, fg, scale);
    }
}

/// Draw a 1px horizontal rule across `[x, x+w)` at row `y`, alpha-scaled by
/// `scale` (0–255) so dim text gets a dim rule too.
fn draw_hline(frame: &mut Frame, x: u32, w: u32, y: i32, rgb: [u8; 3], scale: u16) {
    let Ok(y) = u32::try_from(y) else { return };
    let cov = u8::try_from(scale.min(255)).unwrap_or(255);
    for px in x..x.saturating_add(w) {
        frame.blend(px, y, rgb, cov);
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
            let style = GlyphStyle {
                bold: cell.attrs & ATTR_BOLD != 0,
                italic: cell.attrs & ATTR_ITALIC != 0,
                dim: cell.attrs & ATTR_DIM != 0,
                underline: cell.attrs & ATTR_UNDERLINE != 0,
                strikeout: cell.attrs & ATTR_STRIKEOUT != 0,
            };
            // Bold promotes a dim ANSI base color (0–7) to its bright twin
            // (8–15), matching the conventional `bold_is_bright` terminal
            // behavior — bold text reads as bold even where the font weight is
            // subtle. Only applies to palette indices, never true-color cells.
            let fg_packed = if style.bold && cell.fg <= 7 {
                cell.fg + 8
            } else {
                cell.fg
            };
            let mut fg = resolve_rgb(fg_packed, DEFAULT_FG);
            let mut bg = resolve_rgb(cell.bg, DEFAULT_BG);
            if cell.attrs & ATTR_INVERSE != 0 {
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
                    render_glyph(frame, glyphs, cellbox, c, fg, style);
                } else if style.underline || style.strikeout {
                    // A blank cell can still carry a decoration (e.g. an
                    // underlined run of spaces); draw the rule without a glyph.
                    let cellbox = CellBox {
                        x,
                        y,
                        w: span * ctx.cw,
                    };
                    render_glyph(frame, glyphs, cellbox, ' ', fg, style);
                }
            }
            col += span as usize;
        }
    }
}

/// Overlay ref boxes/markers + numbered badges for the selected nodes.
///
/// The agent-browser-annotated-screenshot look: each box gets a distinct
/// stroke and a small **numbered badge** placed *outside* the box (at its
/// top-left corner) so the badge never covers the content it labels. A legend
/// strip maps each number to its `@ref`. Badges that would collide are nudged
/// apart vertically.
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

    // Badge dimensions: a square sized to the cell height, holding 1–3 digits.
    let badge_h = ctx.ch;
    let mut legend: Vec<(usize, String)> = Vec::new();
    // Track placed badge rects so we can nudge later ones off earlier ones.
    let mut placed: Vec<(u32, u32, u32, u32)> = Vec::new();

    for (i, node) in nodes.iter().enumerate() {
        let Some((arow, acol)) = node.anchor else {
            continue;
        };
        let number = i + 1;
        legend.push((number, node.r#ref.clone()));
        let x = u32::from(acol) * ctx.cw;
        let y = u32::from(arow) * ctx.ch;
        match node.extent {
            Some((ecols, erows)) if ecols > 0 && erows > 0 => {
                // 2px stroke reads cleanly when the still is embedded large.
                frame.draw_rect_outline(
                    x,
                    y,
                    u32::from(ecols) * ctx.cw,
                    u32::from(erows) * ctx.ch,
                    OVERLAY,
                );
                frame.draw_rect_outline(
                    x.saturating_sub(1),
                    y.saturating_sub(1),
                    u32::from(ecols) * ctx.cw + 2,
                    u32::from(erows) * ctx.ch + 2,
                    OVERLAY,
                );
            }
            _ => {
                // No computable span → point marker at the anchor cell.
                frame.fill_rect(x, y, ctx.cw, ctx.ch, OVERLAY);
            }
        }

        // Badge sized to the digit count; placed just outside the box's
        // top-left so it sits adjacent to — not over — the labelled content.
        let label = number.to_string();
        let digits = u32::try_from(label.chars().count()).unwrap_or(1);
        let badge_w = digits * ctx.cw + ctx.cw / 2;
        let mut bx = x.saturating_sub(badge_w);
        // If anchored at the left edge there's no room outside; tuck it just
        // inside the top-left corner instead (still clear of body text rows).
        if x < badge_w {
            bx = x;
        }
        let mut by = y.saturating_sub(badge_h);
        if y < badge_h {
            by = y;
        }
        // Simple collision nudge: push down past any already-placed badge.
        let mut guard = 0;
        while placed
            .iter()
            .any(|&p| rects_overlap((bx, by, badge_w, badge_h), p))
            && guard < 64
        {
            by = by.saturating_add(badge_h);
            guard += 1;
        }
        placed.push((bx, by, badge_w, badge_h));
        draw_badge(frame, glyphs, ctx, bx, by, badge_w, badge_h, &label);
    }

    draw_legend(frame, glyphs, ctx, &legend);
}

/// A pixel rectangle `(x, y, w, h)`.
type Rect = (u32, u32, u32, u32);

/// Axis-aligned overlap test between two `(x, y, w, h)` rectangles.
fn rects_overlap(a: Rect, b: Rect) -> bool {
    let (ax, ay, aw, ah) = a;
    let (bx, by, bw, bh) = b;
    ax < bx + bw && bx < ax + aw && ay < by + bh && by < ay + ah
}

/// Draw a filled rounded badge with centered text in [`LABEL_FG`].
#[allow(clippy::too_many_arguments, clippy::many_single_char_names)]
fn draw_badge(
    frame: &mut Frame,
    glyphs: &mut GlyphCache<'_>,
    ctx: &FontCtx,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    text: &str,
) {
    frame.fill_round_rect(x, y, w, h, (h / 3).max(2), BADGE_BG);
    let n = u32::try_from(text.chars().count()).unwrap_or(0);
    let text_w = n * ctx.cw;
    let tx = x + w.saturating_sub(text_w) / 2;
    for (i, c) in text.chars().enumerate() {
        if !c.is_control() {
            let cellbox = CellBox {
                x: tx + u32::try_from(i).unwrap_or(0) * ctx.cw,
                y,
                w: ctx.cw,
            };
            render_glyph(
                frame,
                glyphs,
                cellbox,
                c,
                LABEL_FG,
                GlyphStyle {
                    bold: true,
                    ..GlyphStyle::default()
                },
            );
        }
    }
}

/// Draw the badge → `@ref` legend down the right margin so each numbered badge
/// is identifiable without overlaying the content. Skipped when there's no
/// room (small grids); badges alone still convey position.
fn draw_legend(
    frame: &mut Frame,
    glyphs: &mut GlyphCache<'_>,
    ctx: &FontCtx,
    legend: &[(usize, String)],
) {
    if legend.is_empty() {
        return;
    }
    // Widest `N @ref` line drives the panel width.
    let max_chars = legend
        .iter()
        .map(|(n, r)| n.to_string().chars().count() + 1 + r.chars().count())
        .max()
        .unwrap_or(0);
    let pad = ctx.cw / 2;
    let panel_w = u32::try_from(max_chars).unwrap_or(0) * ctx.cw + pad * 2;
    let panel_h = u32::try_from(legend.len()).unwrap_or(0) * ctx.ch + pad * 2;
    // Only draw if the panel fits comfortably in the top-right quadrant.
    if panel_w + pad >= frame.width || panel_h + pad >= frame.height {
        return;
    }
    let px = frame.width.saturating_sub(panel_w + pad);
    let py = pad;
    // Translucent dark panel so underlying content stays faintly visible.
    frame.blend_rect(px, py, panel_w, panel_h, [0x14, 0x16, 0x20], 220);
    frame.draw_rect_outline(px, py, panel_w, panel_h, OVERLAY);
    for (row, (number, r#ref)) in legend.iter().enumerate() {
        let ly = py + pad + u32::try_from(row).unwrap_or(0) * ctx.ch;
        let line = format!("{number} {ref}");
        for (i, c) in line.chars().enumerate() {
            if c.is_control() {
                continue;
            }
            let cx = px + pad + u32::try_from(i).unwrap_or(0) * ctx.cw;
            let cellbox = CellBox {
                x: cx,
                y: ly,
                w: ctx.cw,
            };
            // The badge number is bold + accent; the ref is regular off-white.
            let (fg, style) = if i == 0 {
                (
                    OVERLAY,
                    GlyphStyle {
                        bold: true,
                        ..GlyphStyle::default()
                    },
                )
            } else {
                (DEFAULT_FG, GlyphStyle::default())
            };
            render_glyph(frame, glyphs, cellbox, c, fg, style);
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
    // Vibrant modern dark theme (Dracula-class). Replaces the washed VGA-16
    // base palette — the highest visual-impact change for marketing stills.
    // Bright variants (8–15) double as the `bold_is_bright` promotion targets.
    const ANSI: [[u8; 3]; 16] = [
        [0x21, 0x22, 0x2c], // 0  black      (Dracula bg-darker)
        [0xff, 0x55, 0x55], // 1  red
        [0x50, 0xfa, 0x7b], // 2  green
        [0xf1, 0xfa, 0x8c], // 3  yellow
        [0xbd, 0x93, 0xf9], // 4  blue       (Dracula purple-blue)
        [0xff, 0x79, 0xc6], // 5  magenta    (Dracula pink)
        [0x8b, 0xe9, 0xfd], // 6  cyan
        [0xf8, 0xf8, 0xf2], // 7  white      (Dracula foreground)
        [0x62, 0x72, 0xa4], // 8  br black   (Dracula comment)
        [0xff, 0x6e, 0x6e], // 9  br red
        [0x69, 0xff, 0x94], // 10 br green
        [0xff, 0xff, 0xa5], // 11 br yellow
        [0xd6, 0xac, 0xff], // 12 br blue
        [0xff, 0x92, 0xdf], // 13 br magenta
        [0xa4, 0xff, 0xff], // 14 br cyan
        [0xff, 0xff, 0xff], // 15 br white
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
            cursor_visible: true,
            title: None,
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
        let r = render_png(&snap, None, None).expect("render");
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
        let plain = render_png(&snap, None, None).expect("plain");
        let annotated = render_png(
            &snap,
            Some(Annotate {
                outline: &outline,
                selector: None,
            }),
            None,
        )
        .expect("annotated");
        assert!(annotated.annotated);
        assert_ne!(plain.bytes, annotated.bytes);
    }

    #[test]
    fn palette_cube_and_grayscale() {
        // ANSI base-16 is the vibrant modern theme (index 0 is the theme's
        // near-black, 15 is pure white). The 6×6×6 cube + grayscale ramp are
        // unchanged from the xterm-256 spec.
        assert_eq!(palette_rgb(0), [0x21, 0x22, 0x2c]);
        assert_eq!(palette_rgb(15), [0xff, 0xff, 0xff]);
        assert_eq!(palette_rgb(16), [0, 0, 0]);
        assert_eq!(palette_rgb(231), [0xff, 0xff, 0xff]);
        assert_eq!(palette_rgb(232), [8, 8, 8]);
    }

    #[test]
    fn chrome_enlarges_and_changes_output() {
        // The chrome pass composites the grid into a larger framed image:
        // bigger than the bare grid, and byte-different. Bare render stays the
        // canonical `cols*cw × rows*ch`.
        let (cw, ch) = cell_size();
        let snap = snap_row(&[("h", 1, 256, 257), ("i", 1, 1, 257)]);
        let bare = render_png(&snap, None, None).expect("bare");
        assert_eq!(bare.width, 2 * cw);
        assert_eq!(bare.height, ch);
        let opts = ChromeOptions::from_title_arg("git log");
        let framed = render_png(&snap, None, Some(&opts)).expect("framed");
        assert!(framed.width > bare.width, "chrome adds horizontal padding");
        assert!(
            framed.height > bare.height,
            "chrome adds a title bar + padding"
        );
        assert_ne!(bare.bytes, framed.bytes);
        assert_eq!(&framed.bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn attrs_select_distinct_faces() {
        // The same char under different attrs must produce different pixels —
        // proof that bold/italic/dim/underline/strikethrough are honored, not
        // dropped.
        let mk = |attrs: u8| {
            let mut s = snap_row(&[("R", 1, 256, 257)]);
            s.grid.cells[0].attrs = attrs;
            render_decoded(&s)
        };
        let plain = mk(0);
        assert_ne!(plain, mk(ATTR_BOLD), "bold differs from regular");
        assert_ne!(plain, mk(ATTR_ITALIC), "italic differs from regular");
        assert_ne!(plain, mk(ATTR_DIM), "dim differs from regular");
        assert_ne!(plain, mk(ATTR_UNDERLINE), "underline differs from regular");
        assert_ne!(
            plain,
            mk(ATTR_STRIKEOUT),
            "strikethrough differs from regular"
        );
    }

    /// Render to PNG then decode back to raw RGB for pixel assertions.
    fn render_decoded(snap: &EngineSnapshot) -> Vec<u8> {
        let r = render_png(snap, None, None).expect("render");
        let decoder = png::Decoder::new(std::io::Cursor::new(r.bytes));
        let mut reader = decoder.read_info().expect("read_info");
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).expect("next_frame");
        buf.truncate(info.buffer_size());
        buf
    }
}
