//! `alacritty_terminal`-backed [`Engine`] implementation.
//!
//! Wraps `alacritty_terminal::Term` behind a `Mutex` and exposes the trait
//! the daemon talks to. Each `feed` runs bytes through `vte::ansi::Processor`,
//! bumps the per-engine sequence number, and broadcasts a `MutationEvent`.
//!
//! See `docs/RFC.md` §3.3 for the Engine contract and `LEARN-TA-005` for the
//! substrate flip from `wezterm-term` to `alacritty_terminal`.

#![forbid(unsafe_code)]
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::sync::Mutex;

use agent_tui_engine::{
    Cell, CellGrid, Engine, EngineError, EngineSnapshot, ModeFlags, MutationEvent, MutationKind,
    MutationStream, Sequence,
};
use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{Color, Processor, Rgb};
use tokio::sync::broadcast;

/// Headless terminal engine backed by `alacritty_terminal`.
pub struct AlacrittyEngine {
    state: Mutex<State>,
    events: broadcast::Sender<MutationEvent>,
}

/// Mutable engine state guarded by the outer `Mutex`.
struct State {
    term: Term<EventProxy>,
    parser: Processor,
    sequence: Sequence,
    cols: u16,
    rows: u16,
}

#[derive(Clone, Default)]
struct EventProxy;

impl EventListener for EventProxy {
    fn send_event(&self, _event: Event) {
        // The daemon polls `snapshot`/`subscribe` for state; alacritty events
        // (Title changes, PtyWrite, Bell, …) are dropped in v0.1.0.
    }
}

/// `alacritty_terminal::grid::Dimensions` adapter for our `(cols, rows)` pair.
#[derive(Clone, Copy)]
struct Size {
    cols: u16,
    rows: u16,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

impl AlacrittyEngine {
    /// Construct an `AlacrittyEngine` at the given geometry. Both `cols` and
    /// `rows` must be at least 2 / 1 respectively (`alacritty_terminal` will
    /// otherwise panic on resize math).
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        let cols = cols.max(2);
        let rows = rows.max(1);
        let size = Size { cols, rows };
        let term = Term::new(Config::default(), &size, EventProxy);
        let (events, _) = broadcast::channel(256);
        Self {
            state: Mutex::new(State {
                term,
                parser: Processor::default(),
                sequence: 0,
                cols,
                rows,
            }),
            events,
        }
    }

    fn build_snapshot(state: &State) -> EngineSnapshot {
        let grid = state.term.grid();
        let cols = state.cols;
        let rows = state.rows;
        let mut cells = Vec::with_capacity(usize::from(cols) * usize::from(rows));

        for row in 0..i32::from(rows) {
            for col in 0..usize::from(cols) {
                let point = Point::new(Line(row), Column(col));
                let cell = &grid[point];
                cells.push(convert_cell(cell));
            }
        }

        let cursor_point = grid.cursor.point;
        let cursor_row = u16::try_from(cursor_point.line.0.max(0)).unwrap_or(u16::MAX);
        let cursor_col = u16::try_from(cursor_point.column.0).unwrap_or(u16::MAX);

        EngineSnapshot {
            grid: CellGrid {
                cols,
                rows,
                cells,
                cursor: (cursor_row, cursor_col),
            },
            modes: mode_flags(*state.term.mode()),
            sequence: state.sequence,
        }
    }

    fn emit(&self, sequence: Sequence, kind: MutationKind) {
        // Send failures (no subscribers) are not engine errors.
        let _ = self.events.send(MutationEvent { sequence, kind });
    }
}

impl Engine for AlacrittyEngine {
    fn feed(&self, bytes: &[u8]) -> Result<(), EngineError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut s = self
            .state
            .lock()
            .map_err(|e| EngineError::Refused(format!("poisoned: {e}")))?;
        // Split-borrow: parser and term are sibling fields of `s`.
        let State { parser, term, .. } = &mut *s;
        parser.advance(term, bytes);
        s.sequence = s.sequence.saturating_add(1);
        let seq = s.sequence;
        drop(s);
        self.emit(seq, MutationKind::Output);
        Ok(())
    }

    fn snapshot(&self) -> EngineSnapshot {
        let s = self.state.lock().expect("engine state lock poisoned");
        Self::build_snapshot(&s)
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), EngineError> {
        let cols = cols.max(2);
        let rows = rows.max(1);
        let mut s = self
            .state
            .lock()
            .map_err(|e| EngineError::Refused(format!("poisoned: {e}")))?;
        s.term.resize(Size { cols, rows });
        s.cols = cols;
        s.rows = rows;
        s.sequence = s.sequence.saturating_add(1);
        let seq = s.sequence;
        drop(s);
        self.emit(seq, MutationKind::Resize);
        Ok(())
    }

    fn subscribe(&self) -> MutationStream {
        self.events.subscribe()
    }
}

fn convert_cell(cell: &alacritty_terminal::term::cell::Cell) -> Cell {
    let width = cell_width(cell.flags);
    Cell {
        ch: cell.c.to_string(),
        width,
        fg: encode_color(cell.fg),
        bg: encode_color(cell.bg),
        attrs: pack_attrs(cell.flags),
    }
}

/// Display width: 2 for primary wide cells, 0 for the spacer, 1 otherwise.
#[allow(clippy::bool_to_int_with_if)]
fn cell_width(flags: Flags) -> u8 {
    if flags.contains(Flags::WIDE_CHAR) {
        2
    } else if flags.contains(Flags::WIDE_CHAR_SPACER) {
        0
    } else {
        1
    }
}

/// Pack `alacritty_terminal::Color` into a `u32`:
/// - bit 24 set → 24-bit RGB in low 24 bits (0xRRGGBB)
/// - otherwise → palette index (0..=255)
fn encode_color(color: Color) -> u32 {
    match color {
        Color::Named(n) => n as u32,
        Color::Indexed(i) => u32::from(i),
        Color::Spec(Rgb { r, g, b }) => {
            0x0100_0000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
        }
    }
}

/// Pack alacritty `Flags` into our `u8` attribute byte:
/// bit 0 = bold, 1 = italic, 2 = underline (any), 3 = inverse, 4 = strikeout.
fn pack_attrs(flags: Flags) -> u8 {
    let mut a = 0u8;
    if flags.contains(Flags::BOLD) {
        a |= 1 << 0;
    }
    if flags.contains(Flags::ITALIC) {
        a |= 1 << 1;
    }
    if flags.intersects(Flags::ALL_UNDERLINES) {
        a |= 1 << 2;
    }
    if flags.contains(Flags::INVERSE) {
        a |= 1 << 3;
    }
    if flags.contains(Flags::STRIKEOUT) {
        a |= 1 << 4;
    }
    a
}

fn mode_flags(mode: TermMode) -> ModeFlags {
    ModeFlags {
        alt_screen: mode.contains(TermMode::ALT_SCREEN),
        bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
        // alacritty exposes mouse modes individually; treat 1003 (motion) as
        // the umbrella signal — agents only need to know "is mouse tracking on".
        mouse_1003: mode.contains(TermMode::MOUSE_MOTION),
        kkp_active: mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL),
        kkp_flags: kkp_bits(mode),
    }
}

fn kkp_bits(mode: TermMode) -> u32 {
    let mut bits = 0u32;
    if mode.contains(TermMode::DISAMBIGUATE_ESC_CODES) {
        bits |= 1 << 0;
    }
    if mode.contains(TermMode::REPORT_EVENT_TYPES) {
        bits |= 1 << 1;
    }
    if mode.contains(TermMode::REPORT_ALTERNATE_KEYS) {
        bits |= 1 << 2;
    }
    if mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) {
        bits |= 1 << 3;
    }
    if mode.contains(TermMode::REPORT_ASSOCIATED_TEXT) {
        bits |= 1 << 4;
    }
    bits
}
