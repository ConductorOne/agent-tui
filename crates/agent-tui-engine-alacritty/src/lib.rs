//! `alacritty_terminal`-backed [`Engine`] implementation.
//!
//! Wraps `alacritty_terminal::Term` behind a `Mutex` and exposes the trait
//! the daemon talks to. Each `feed` runs bytes through `vte::ansi::Processor`,
//! bumps the per-engine sequence number, and broadcasts a `MutationEvent`.
//!
//! See `docs/design/RFC.md` §3.3 for the Engine contract and `LEARN-TA-005` for the
//! substrate flip from `wezterm-term` to `alacritty_terminal`.

#![forbid(unsafe_code)]
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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
    /// Out-of-band terminal events (bell rings, title changes) captured by
    /// the [`EventProxy`] while the parser advances. Shared with the proxy
    /// installed in the `Term`.
    term_events: Arc<TermEvents>,
}

/// Mutable engine state guarded by the outer `Mutex`.
struct State {
    term: Term<EventProxy>,
    parser: Processor,
    sequence: Sequence,
    cols: u16,
    rows: u16,
    /// Carry-over for an incomplete UTF-8 codepoint at the tail of the
    /// previous `feed` call. Upstream `alacritty_terminal::Processor`
    /// doesn't preserve UTF-8 buffer state across `advance()` calls —
    /// when a multi-byte codepoint is split (e.g. by a 4KB PTY read
    /// boundary), the second chunk's continuation bytes get treated as
    /// stand-alone control bytes. We buffer the lead bytes here and
    /// merge them into the next feed before forwarding.
    ///
    /// Capacity is `<= 3` because the longest UTF-8 prefix that's still
    /// incomplete is 3 bytes (waiting for the 4th continuation).
    utf8_carry: Vec<u8>,
}

/// Shared sink for the alacritty events agents care about. The
/// [`EventProxy`] writes into it from inside `parser.advance()` (under the
/// engine's state lock); `feed` inspects deltas after each advance and
/// `build_snapshot` reads the current title.
#[derive(Default)]
struct TermEvents {
    /// Cumulative BEL count. `feed` diffs this across an advance to know
    /// how many bells the chunk rang.
    bell_count: AtomicU64,
    /// Most recent OSC 0/2 title; `None` after a reset (or never set).
    title: Mutex<Option<String>>,
    /// Bytes the terminal must write back to the PTY: DSR/DA/KKP replies.
    /// Accumulated from `Event::PtyWrite` during `parser.advance`, drained by
    /// `take_pty_writes` after each feed and forwarded to the PTY by the daemon.
    pty_writes: Mutex<Vec<u8>>,
}

/// Listener installed in the `Term`; forwards the events agents care about
/// into the shared [`TermEvents`] sink and drops the rest (`PtyWrite`,
/// clipboard, …) — the daemon polls `snapshot`/`subscribe` for state.
#[derive(Clone, Default)]
struct EventProxy(Arc<TermEvents>);

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Bell => {
                self.0.bell_count.fetch_add(1, Ordering::Relaxed);
            }
            Event::Title(title) => {
                *self.0.title.lock().expect("title lock poisoned") = Some(title);
            }
            Event::ResetTitle => {
                *self.0.title.lock().expect("title lock poisoned") = None;
            }
            Event::PtyWrite(text) => {
                self.0
                    .pty_writes
                    .lock()
                    .expect("pty_writes lock poisoned")
                    .extend_from_slice(text.as_bytes());
            }
            _ => {}
        }
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
        let term_events = Arc::new(TermEvents::default());
        let term = Term::new(Config::default(), &size, EventProxy(term_events.clone()));
        let (events, _) = broadcast::channel(256);
        Self {
            state: Mutex::new(State {
                term,
                parser: Processor::default(),
                sequence: 0,
                cols,
                rows,
                utf8_carry: Vec::with_capacity(3),
            }),
            events,
            term_events,
        }
    }

    fn build_snapshot(state: &State, term_events: &TermEvents) -> EngineSnapshot {
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
            cursor_visible: state.term.mode().contains(TermMode::SHOW_CURSOR),
            title: term_events
                .title
                .lock()
                .expect("title lock poisoned")
                .clone(),
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

    /// Stitch any UTF-8 carry-over from the previous feed onto the
    /// current input and split off a new tail of incomplete bytes.
    /// Returns the bytes safe to forward to the parser.
    ///
    /// Why: `alacritty_terminal::Processor::advance` doesn't preserve
    /// UTF-8 continuation state across calls. A 4KB PTY read can split
    /// a multi-byte codepoint at any byte; without this, the second
    /// chunk's continuation bytes get treated as stray C1 controls and
    /// the cell grid drifts. See `feed_is_associative_for_utf8` in
    /// `tests/proptest_invariants.rs` — that property exercises this.
    fn buffer_utf8_tail(carry: &mut Vec<u8>, input: &[u8]) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::with_capacity(carry.len() + input.len());
        buf.extend_from_slice(carry);
        buf.extend_from_slice(input);
        carry.clear();

        // Walk backwards from the tail looking for an incomplete UTF-8
        // lead byte. UTF-8 lead patterns: 0xC0..=0xDF (2-byte),
        // 0xE0..=0xEF (3-byte), 0xF0..=0xF7 (4-byte). A continuation
        // byte is 0x80..=0xBF.
        let max_lookback = buf.len().min(3);
        for back in 1..=max_lookback {
            let i = buf.len() - back;
            let b = buf[i];
            let needed: usize = if b < 0x80 {
                // ASCII or stray continuation; not a lead.
                continue;
            } else if (0xC0..=0xDF).contains(&b) {
                2
            } else if (0xE0..=0xEF).contains(&b) {
                3
            } else if (0xF0..=0xF7).contains(&b) {
                4
            } else {
                // Stray continuation byte (0x80..=0xBF) or invalid lead
                // (>=0xF8). Don't buffer — let the parser decide.
                continue;
            };
            let trailing = buf.len() - i;
            if trailing < needed {
                // Incomplete lead at offset `i` — buffer it for next feed.
                let tail = buf.split_off(i);
                carry.extend_from_slice(&tail);
                break;
            }
            // Otherwise the lead has enough continuation bytes; nothing
            // to buffer.
            break;
        }
        buf
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
        // Capture pre-advance state so this chunk's side effects (mode
        // flips, bell rings) can be diffed and broadcast as their own
        // mutation kinds after the parse.
        let modes_before = mode_flags(*s.term.mode());
        let bells_before = self.term_events.bell_count.load(Ordering::Relaxed);
        let safe = Self::buffer_utf8_tail(&mut s.utf8_carry, bytes);
        if !safe.is_empty() {
            // Split-borrow: parser and term are sibling fields of `s`.
            let State { parser, term, .. } = &mut *s;
            parser.advance(term, &safe);
        }
        s.sequence = s.sequence.saturating_add(1);
        let seq = s.sequence;
        let modes_changed = mode_flags(*s.term.mode()) != modes_before;
        drop(s);
        self.emit(seq, MutationKind::Output);
        // Same sequence for the companion events: they're facets of the one
        // mutation this feed applied (sequence is monotonic, not strict).
        if modes_changed {
            self.emit(seq, MutationKind::ModeChange);
        }
        let bells = self
            .term_events
            .bell_count
            .load(Ordering::Relaxed)
            .saturating_sub(bells_before);
        for _ in 0..bells {
            self.emit(seq, MutationKind::Bell);
        }
        Ok(())
    }

    fn snapshot(&self) -> EngineSnapshot {
        let s = self.state.lock().expect("engine state lock poisoned");
        Self::build_snapshot(&s, &self.term_events)
    }

    fn dimensions(&self) -> (u16, u16) {
        let s = self.state.lock().expect("engine state lock poisoned");
        (s.cols, s.rows)
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

    fn take_pty_writes(&self) -> Vec<u8> {
        std::mem::take(
            &mut *self
                .term_events
                .pty_writes
                .lock()
                .expect("pty_writes lock poisoned"),
        )
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
/// bit 0 = bold, 1 = italic, 2 = underline (any), 3 = inverse, 4 = strikeout,
/// bit 5 = dim/faint.
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
    if flags.contains(Flags::DIM) {
        a |= 1 << 5;
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
