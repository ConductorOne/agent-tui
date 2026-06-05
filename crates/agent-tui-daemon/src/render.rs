//! `snapshot --png` rasterization.
//!
//! Renders the engine's cell grid to an RGB framebuffer — one fixed
//! [`CELL_W`]×[`CELL_H`] cell per grid cell, glyphs from the embedded
//! `font8x8` bitmap, fg/bg resolved from the same packed-color encoding the
//! `cells`/`text` modes use — then PNG-encodes it. With `--annotate`, ref
//! bounding boxes (anchor → anchor+extent) and `@ref` labels are overlaid;
//! refs without a computable extent fall back to a point marker + label.
//!
//! Deliberately dependency-light: `png` (pure-Rust deflate) for encoding and
//! `font8x8` (const bitmap data) for glyphs — no full image-codec stack.

use agent_tui_engine::EngineSnapshot;
use agent_tui_protocol::{Outline, OutlineNode, Selector};
use font8x8::{BASIC_FONTS, UnicodeFonts};

/// Pixel width of one terminal cell (matches the 8×8 glyph bitmap).
pub const CELL_W: u32 = 8;
/// Pixel height of one terminal cell.
pub const CELL_H: u32 = 8;

/// Default foreground when a cell carries the terminal's default fg.
const DEFAULT_FG: [u8; 3] = [0xd0, 0xd0, 0xd0];
/// Default background when a cell carries the terminal's default bg.
const DEFAULT_BG: [u8; 3] = [0x0a, 0x0a, 0x0a];
/// Overlay color for `--annotate` boxes, markers, and label backgrounds.
pub const OVERLAY: [u8; 3] = [0x14, 0xd4, 0x4a];
/// Label text color (drawn on the [`OVERLAY`] strip).
const LABEL_FG: [u8; 3] = [0x00, 0x00, 0x00];

/// A rasterized snapshot: PNG bytes plus the image geometry.
pub struct RenderedPng {
    /// PNG-encoded image bytes, ready to write to disk.
    pub bytes: Vec<u8>,
    /// Image width in pixels (`cols * CELL_W`).
    pub width: u32,
    /// Image height in pixels (`rows * CELL_H`).
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
    let cols = u32::from(snap.grid.cols);
    let rows = u32::from(snap.grid.rows);
    if cols == 0 || rows == 0 {
        return Err("cannot rasterize an empty (0×0) grid".to_string());
    }
    let width = cols * CELL_W;
    let height = rows * CELL_H;

    let mut frame = Frame::new(width, height, DEFAULT_BG);
    paint_cells(&mut frame, snap);

    let annotated = if let Some(a) = annotate {
        paint_overlay(&mut frame, &a);
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

    fn fill_rect(&mut self, x0: u32, y0: u32, w: u32, h: u32, rgb: [u8; 3]) {
        for y in y0..y0.saturating_add(h) {
            for x in x0..x0.saturating_add(w) {
                self.put(x, y, rgb);
            }
        }
    }

    /// Blit an 8×8 glyph for `ch` at `(x0, y0)` in color `fg`. Unknown glyphs
    /// (no bitmap) draw nothing.
    fn draw_glyph(&mut self, x0: u32, y0: u32, ch: char, fg: [u8; 3]) {
        let Some(bitmap) = BASIC_FONTS.get(ch) else {
            return;
        };
        for (row, bits) in bitmap.iter().enumerate() {
            let ry = u32::try_from(row).unwrap_or(0);
            for col in 0..8u32 {
                // font8x8 packs the leftmost pixel in the least-significant bit.
                if bits & (1 << col) != 0 {
                    self.put(x0 + col, y0 + ry, fg);
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

    /// Draw `text` as a label: a filled background strip with glyphs on top.
    /// Clamped so a label anchored at the top edge stays visible.
    fn draw_label(&mut self, x: u32, y: u32, text: &str, fg: [u8; 3], bg: [u8; 3]) {
        let n = u32::try_from(text.chars().count()).unwrap_or(0);
        if n == 0 {
            return;
        }
        let w = n * CELL_W;
        // Keep the strip on-screen horizontally and vertically.
        let lx = x.min(self.width.saturating_sub(w.min(self.width)));
        let ly = y.min(self.height.saturating_sub(CELL_H.min(self.height)));
        self.fill_rect(lx, ly, w, CELL_H, bg);
        for (i, ch) in text.chars().enumerate() {
            let cx = lx + u32::try_from(i).unwrap_or(0) * CELL_W;
            self.draw_glyph(cx, ly, ch, fg);
        }
    }
}

/// Paint every grid cell's background + glyph into the frame.
fn paint_cells(frame: &mut Frame, snap: &EngineSnapshot) {
    let cols = usize::from(snap.grid.cols);
    let rows = usize::from(snap.grid.rows);
    for row in 0..rows {
        for col in 0..cols {
            let cell = &snap.grid.cells[row * cols + col];
            let mut fg = resolve_rgb(cell.fg, DEFAULT_FG);
            let mut bg = resolve_rgb(cell.bg, DEFAULT_BG);
            // attrs bit 3 = inverse (see `pack_attrs` in the alacritty engine).
            if cell.attrs & (1 << 3) != 0 {
                std::mem::swap(&mut fg, &mut bg);
            }
            let x = u32::try_from(col).unwrap_or(0) * CELL_W;
            let y = u32::try_from(row).unwrap_or(0) * CELL_H;
            frame.fill_rect(x, y, CELL_W, CELL_H, bg);
            if let Some(ch) = cell.ch.chars().next() {
                if ch != ' ' && !ch.is_control() {
                    frame.draw_glyph(x, y, ch, fg);
                }
            }
        }
    }
}

/// Overlay ref boxes/markers + labels for the selected nodes.
fn paint_overlay(frame: &mut Frame, a: &Annotate<'_>) {
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
        let x = u32::from(acol) * CELL_W;
        let y = u32::from(arow) * CELL_H;
        match node.extent {
            Some((ecols, erows)) if ecols > 0 && erows > 0 => {
                frame.draw_rect_outline(
                    x,
                    y,
                    u32::from(ecols) * CELL_W,
                    u32::from(erows) * CELL_H,
                    OVERLAY,
                );
            }
            _ => {
                // No computable span → point marker at the anchor cell.
                frame.fill_rect(x, y, CELL_W, CELL_H, OVERLAY);
            }
        }
        frame.draw_label(x, y, &node.r#ref, LABEL_FG, OVERLAY);
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

    fn snap_2x1() -> EngineSnapshot {
        EngineSnapshot {
            grid: CellGrid {
                cols: 2,
                rows: 1,
                cells: vec![
                    Cell {
                        ch: "h".into(),
                        width: 1,
                        fg: 256,
                        bg: 257,
                        attrs: 0,
                    },
                    Cell {
                        ch: "i".into(),
                        width: 1,
                        fg: 1,
                        bg: 257,
                        attrs: 0,
                    },
                ],
                cursor: (0, 0),
            },
            modes: ModeFlags::default(),
            sequence: 0,
        }
    }

    #[test]
    fn renders_png_with_expected_dims() {
        let r = render_png(&snap_2x1(), None).expect("render");
        assert_eq!(r.width, 2 * CELL_W);
        assert_eq!(r.height, CELL_H);
        assert!(!r.annotated);
        // PNG magic number.
        assert_eq!(&r.bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn annotate_changes_output() {
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
        let plain = render_png(&snap_2x1(), None).expect("plain");
        let annotated = render_png(
            &snap_2x1(),
            Some(Annotate {
                outline: &outline,
                selector: None,
            }),
        )
        .expect("annotated");
        assert!(annotated.annotated);
        assert_ne!(
            plain.bytes, annotated.bytes,
            "overlay must change the image"
        );
    }

    #[test]
    fn palette_cube_and_grayscale() {
        assert_eq!(palette_rgb(0), [0, 0, 0]);
        assert_eq!(palette_rgb(15), [0xff, 0xff, 0xff]);
        assert_eq!(palette_rgb(16), [0, 0, 0]); // cube origin
        assert_eq!(palette_rgb(231), [0xff, 0xff, 0xff]); // cube max
        assert_eq!(palette_rgb(232), [8, 8, 8]); // gray ramp start
    }
}
