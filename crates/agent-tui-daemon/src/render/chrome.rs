//! Window-chrome post-process pass for `snapshot --png --chrome`.
//!
//! Composites a painted terminal cell grid into a marketing-grade still: a
//! brand-dark backdrop, a drop shadow, a rounded-corner window body, a title
//! bar with macOS-style traffic-light dots and optional centered title text,
//! generous padding, and the grid pasted into the window body. This is the
//! "polished terminal screenshot" treatment (Carbon / ray.so / freeze class).
//!
//! Runs *after* the cell grid is rasterized, on the RGB framebuffer the cell
//! renderer produced — so the renderer's cell/text/color output is untouched
//! and stays byte-stable when chrome is off. Uses `tiny-skia` (BSD-3-Clause,
//! pure-Rust 2D raster); the result is re-encoded through the existing `png`
//! crate by the caller.

// 2D raster geometry inherently converts between integer pixel coordinates and
// the f32 path/canvas space; the values are small layout offsets (sub-4096),
// so the precision/truncation/sign casts these lints flag are benign here.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    // x/y/w/h/r and cx/cy/dx/dy are the conventional names for 2D geometry.
    clippy::many_single_char_names
)]

use tiny_skia::{
    Color, FillRule, FilterQuality, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Transform,
};

/// Tunables for the chrome pass. Defaults render the look in
/// `gitlog-after-mockup.png`.
#[derive(Clone, Debug, Default)]
pub struct ChromeOptions {
    /// Title-bar text. `None` falls back to the terminal's OSC title, then
    /// empty (traffic lights only, no text).
    pub title: Option<String>,
}

impl ChromeOptions {
    /// Build chrome options from the CLI `--chrome [TITLE]` value: an empty
    /// string means "frame, no explicit title" (falls back to the OSC title).
    #[must_use]
    pub fn from_title_arg(title: &str) -> Self {
        let title = if title.trim().is_empty() {
            None
        } else {
            Some(title.to_string())
        };
        Self { title }
    }
}

/// A finished framed image: RGB bytes + geometry, ready to PNG-encode.
pub(super) struct Framed {
    pub buf: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

// --- Layout + palette ---------------------------------------------------

/// Outer padding (backdrop margin) around the whole window, in px.
const PAD: u32 = 48;
/// Title-bar height, in px.
const TITLE_BAR: u32 = 40;
/// Inner padding between the title bar / window edges and the cell grid.
const BODY_PAD: u32 = 18;
/// Window-body corner radius, in px.
const RADIUS: f32 = 12.0;
/// Drop-shadow blur extent (how far the shadow bleeds), in px.
const SHADOW: u32 = 28;

/// Brand-dark backdrop behind the window (deep indigo-black).
const BACKDROP: [u8; 3] = [0x16, 0x18, 0x22];
/// Title-bar fill (slightly lighter than the body for separation).
const TITLE_BG: [u8; 3] = [0x2c, 0x2e, 0x3c];
/// Window-body fill (matches the renderer's default cell background so the
/// grid melts into the body with no seam).
const BODY_BG: [u8; 3] = [0x28, 0x2a, 0x36];
/// Title text color.
const TITLE_FG: [u8; 3] = [0x9a, 0x9c, 0xb0];
/// Traffic-light dot colors: close / minimize / zoom.
const DOTS: [[u8; 3]; 3] = [[0xff, 0x5f, 0x57], [0xfe, 0xbc, 0x2e], [0x28, 0xc8, 0x40]];

fn color(rgb: [u8; 3]) -> Color {
    Color::from_rgba8(rgb[0], rgb[1], rgb[2], 255)
}

/// Composite `grid` (RGB, `grid_w × grid_h`) into a framed still. `title` is
/// the already-resolved title-bar text (the caller folds in [`ChromeOptions`]
/// and the terminal's OSC title).
#[must_use]
pub(super) fn composite(grid: &[u8], grid_w: u32, grid_h: u32, title: &str) -> Framed {
    // Window = title bar + padded body around the grid.
    let win_w = grid_w + BODY_PAD * 2;
    let win_h = TITLE_BAR + grid_h + BODY_PAD * 2;
    // Full canvas = window + outer padding (with extra room below for the
    // offset drop shadow).
    let width = win_w + PAD * 2;
    let height = win_h + PAD * 2;

    let mut canvas =
        Pixmap::new(width, height).expect("chrome canvas dimensions are valid (non-zero)");
    canvas.fill(color(BACKDROP));

    let win_x = PAD as f32;
    let win_y = PAD as f32;

    // 1. Drop shadow — a blurred, offset dark rounded rect under the window.
    draw_shadow(&mut canvas, win_x, win_y, win_w as f32, win_h as f32);

    // 2. Window body — rounded rect filled with the body bg.
    fill_round_rect(
        &mut canvas,
        win_x,
        win_y,
        win_w as f32,
        win_h as f32,
        RADIUS,
        color(BODY_BG),
    );

    // 3. Title bar — same rounded rect clipped to the top strip (a rounded-top
    //    band). Approximated by a rounded rect of the bar height overlaid by a
    //    square-bottomed rect so only the top corners are round.
    fill_round_rect(
        &mut canvas,
        win_x,
        win_y,
        win_w as f32,
        TITLE_BAR as f32 + RADIUS,
        RADIUS,
        color(TITLE_BG),
    );
    if let Some(r) = Rect::from_xywh(win_x, win_y + TITLE_BAR as f32, win_w as f32, RADIUS) {
        let mut p = Paint::default();
        p.set_color(color(BODY_BG));
        p.anti_alias = false;
        canvas.fill_rect(r, &p, Transform::identity(), None);
    }

    // 4. Traffic-light dots, left-aligned in the title bar.
    let dot_r = 6.0;
    let dot_cy = win_y + TITLE_BAR as f32 / 2.0;
    for (i, c) in DOTS.iter().enumerate() {
        let cx = win_x + 20.0 + i as f32 * 20.0;
        fill_circle(&mut canvas, cx, dot_cy, dot_r, color(*c));
    }

    // 5. Title text, centered in the title bar.
    if !title.is_empty() {
        draw_title(&mut canvas, title, win_x, win_y, win_w as f32);
    }

    // 6. The terminal grid, pasted into the window body.
    let grid_x = win_x as u32 + BODY_PAD;
    let grid_y = win_y as u32 + TITLE_BAR + BODY_PAD;
    paste_grid(&mut canvas, grid, grid_w, grid_h, grid_x, grid_y);

    // Down-convert RGBA → RGB for the existing png encoder.
    let rgba = canvas.data();
    let mut buf = Vec::with_capacity((width * height * 3) as usize);
    for px in rgba.chunks_exact(4) {
        buf.extend_from_slice(&px[..3]);
    }
    Framed { buf, width, height }
}

/// Paste an RGB grid into the canvas at `(x, y)` via a temporary pixmap.
fn paste_grid(canvas: &mut Pixmap, grid: &[u8], w: u32, h: u32, x: u32, y: u32) {
    let mut tile = Pixmap::new(w, h).expect("grid dimensions are valid");
    let data = tile.data_mut();
    for (i, src) in grid.chunks_exact(3).enumerate() {
        let d = i * 4;
        data[d] = src[0];
        data[d + 1] = src[1];
        data[d + 2] = src[2];
        data[d + 3] = 255;
    }
    let paint = PixmapPaint {
        quality: FilterQuality::Nearest,
        ..PixmapPaint::default()
    };
    canvas.draw_pixmap(
        x as i32,
        y as i32,
        tile.as_ref(),
        &paint,
        Transform::identity(),
        None,
    );
}

/// Fill a rounded rectangle in `paint_color`.
fn fill_round_rect(canvas: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color) {
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        canvas.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// Fill a circle.
fn fill_circle(canvas: &mut Pixmap, cx: f32, cy: f32, r: f32, color: Color) {
    let mut pb = PathBuilder::new();
    pb.push_circle(cx, cy, r);
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        canvas.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// Draw a soft drop shadow under the window: concentric translucent rounded
/// rects, offset down, giving a cheap blur without a full gaussian pass.
fn draw_shadow(canvas: &mut Pixmap, x: f32, y: f32, w: f32, h: f32) {
    let offset = 10.0;
    let layers = SHADOW;
    for i in (1..=layers).rev() {
        let grow = i as f32;
        let alpha = (40.0 / layers as f32) as u8;
        let mut pb = PathBuilder::new();
        let sx = x - grow;
        let sy = y - grow + offset;
        let sw = w + grow * 2.0;
        let sh = h + grow * 2.0;
        let r = RADIUS + grow;
        pb.move_to(sx + r, sy);
        pb.line_to(sx + sw - r, sy);
        pb.quad_to(sx + sw, sy, sx + sw, sy + r);
        pb.line_to(sx + sw, sy + sh - r);
        pb.quad_to(sx + sw, sy + sh, sx + sw - r, sy + sh);
        pb.line_to(sx + r, sy + sh);
        pb.quad_to(sx, sy + sh, sx, sy + sh - r);
        pb.line_to(sx, sy + r);
        pb.quad_to(sx, sy, sx + r, sy);
        pb.close();
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color(Color::from_rgba8(0, 0, 0, alpha.max(1)));
            paint.anti_alias = true;
            canvas.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
}

/// Draw centered title text using the embedded Regular face, rasterized
/// through the parent module's font context (so the title matches the grid's
/// typeface). Kept minimal — a single regular weight, centered.
fn draw_title(canvas: &mut Pixmap, title: &str, win_x: f32, win_y: f32, win_w: f32) {
    use super::{PX, font_ctx};
    let ctx = font_ctx();
    let cw = ctx.cw;
    let text_w = u32::try_from(title.chars().count()).unwrap_or(0) * cw;
    // Center horizontally in the title bar; vertically center the cap height.
    let tx = win_x + (win_w - text_w as f32) / 2.0;
    let ty = win_y + (TITLE_BAR as f32) / 2.0;
    let face = &ctx.faces[0];
    let mut pen_x = tx.max(win_x);
    for ch in title.chars() {
        if ch.is_control() {
            continue;
        }
        let (m, bitmap) = face.rasterize(ch, PX);
        // Baseline so the glyph centers on `ty`.
        let baseline = ty + (PX / 2.0) * 0.5;
        let gx0 = pen_x + m.xmin as f32;
        let gy0 = baseline - m.height as f32 - m.ymin as f32;
        blit_coverage(canvas, &bitmap, m.width, m.height, gx0, gy0, TITLE_FG);
        pen_x += cw as f32;
    }
}

/// Alpha-blend a fontdue coverage bitmap onto the canvas in `rgb`.
fn blit_coverage(
    canvas: &mut Pixmap,
    bitmap: &[u8],
    bw: usize,
    bh: usize,
    x0: f32,
    y0: f32,
    rgb: [u8; 3],
) {
    let cw = canvas.width();
    let ch = canvas.height();
    let data = canvas.data_mut();
    for row in 0..bh {
        for col in 0..bw {
            let cov = bitmap[row * bw + col];
            if cov == 0 {
                continue;
            }
            let px = x0 as i32 + col as i32;
            let py = y0 as i32 + row as i32;
            if px < 0 || py < 0 {
                continue;
            }
            let (px, py) = (px as u32, py as u32);
            if px >= cw || py >= ch {
                continue;
            }
            let idx = ((py * cw + px) * 4) as usize;
            let a = u16::from(cov);
            let inv = 255 - a;
            for k in 0..3 {
                let dst = u16::from(data[idx + k]);
                let src = u16::from(rgb[k]);
                data[idx + k] = u8::try_from((src * a + dst * inv) / 255).unwrap_or(255);
            }
            // Keep the canvas opaque.
            data[idx + 3] = 255;
        }
    }
}
