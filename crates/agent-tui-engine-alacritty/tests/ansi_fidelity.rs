//! Fidelity tests for `AlacrittyEngine`.
//!
//! Each test catches a real regression class:
//!  - text rendering (would break every snapshot if regressed)
//!  - CRLF / line wrap (would break shell prompt detection)
//!  - alt-screen mode flag (would break the state classifier in P1)
//!  - cursor positioning (would break wait --cursor-stable in P1)
//!  - wide CJK char width (would break ref column math)
//!  - sequence + event emission (foundation for wait subsystem in P1)
//!  - canonical hash determinism (foundation for wait --hash in P1)
//!  - resize semantics + mutation event

use agent_tui_engine::{Engine, MutationKind};
use agent_tui_engine_alacritty::AlacrittyEngine;

/// Read the first `n` chars of row 0 from a snapshot into a `String`.
fn row0_prefix(eng: &AlacrittyEngine, n: usize) -> String {
    let snap = eng.snapshot();
    snap.grid
        .cells
        .iter()
        .take(n)
        .map(|c| c.ch.as_str())
        .collect()
}

#[test]
fn plain_text_renders_into_row_zero() {
    let eng = AlacrittyEngine::new(20, 4);
    eng.feed(b"hello").expect("feed ok");
    assert_eq!(row0_prefix(&eng, 5), "hello");
}

#[test]
fn crlf_wraps_to_next_row() {
    let eng = AlacrittyEngine::new(20, 4);
    eng.feed(b"a\r\nb").expect("feed ok");
    let snap = eng.snapshot();
    let cols = usize::from(snap.grid.cols);
    assert_eq!(snap.grid.cells[0].ch, "a", "row 0 col 0");
    assert_eq!(snap.grid.cells[cols].ch, "b", "row 1 col 0");
}

#[test]
fn alt_screen_1049_toggles_mode_flag() {
    let eng = AlacrittyEngine::new(20, 4);
    assert!(!eng.snapshot().modes.alt_screen, "starts off");
    eng.feed(b"\x1b[?1049h").expect("enter alt-screen");
    assert!(eng.snapshot().modes.alt_screen, "alt on after 1049h");
    eng.feed(b"\x1b[?1049l").expect("leave alt-screen");
    assert!(!eng.snapshot().modes.alt_screen, "alt off after 1049l");
}

#[test]
fn cup_moves_cursor() {
    let eng = AlacrittyEngine::new(20, 8);
    // CUP: ESC [ row ; col H — 1-indexed.
    eng.feed(b"\x1b[3;5H").expect("CUP ok");
    let snap = eng.snapshot();
    assert_eq!(
        snap.grid.cursor,
        (2, 4),
        "cursor at row 2 col 4 (0-indexed)"
    );
}

#[test]
fn wide_cjk_char_takes_two_cells() {
    let eng = AlacrittyEngine::new(20, 4);
    eng.feed("日".as_bytes()).expect("feed ok");
    let snap = eng.snapshot();
    assert_eq!(snap.grid.cells[0].ch, "日", "primary cell holds the glyph");
    assert_eq!(snap.grid.cells[0].width, 2, "primary cell width = 2");
    assert_eq!(snap.grid.cells[1].width, 0, "spacer cell width = 0");
}

#[tokio::test]
async fn feed_bumps_sequence_and_emits_event() {
    let eng = AlacrittyEngine::new(20, 4);
    let mut sub = eng.subscribe();
    let s0 = eng.snapshot().sequence;
    eng.feed(b"hello").expect("feed ok");
    let evt = sub.recv().await.expect("event arrives");
    assert_eq!(evt.sequence, s0 + 1, "sequence bumped by 1");
    assert_eq!(evt.kind, MutationKind::Output, "output mutation");
    assert_eq!(eng.snapshot().sequence, s0 + 1, "snapshot.sequence matches");
}

#[test]
fn canonical_hash_is_deterministic_after_identical_feeds() {
    let bytes = b"\x1b[31mred\x1b[0m \xe6\x97\xa5\r\nsecond row";
    let a = AlacrittyEngine::new(40, 6);
    let b = AlacrittyEngine::new(40, 6);
    a.feed(bytes).unwrap();
    b.feed(bytes).unwrap();
    assert_eq!(
        a.snapshot().canonical_hash(),
        b.snapshot().canonical_hash(),
        "two engines fed identical bytes must hash equal"
    );
}

/// Regression test for the UTF-8 stitching across `feed` boundaries.
///
/// `alacritty_terminal::Processor::advance` doesn't buffer incomplete
/// UTF-8 across calls; without our wrapper's `utf8_carry` field, a
/// 4-byte sequence split mid-codepoint produced different cursor state
/// than the atomic feed. See proptest `feed_is_associative_for_utf8`
/// — this is the human-readable narrow case.
#[test]
fn utf8_codepoint_split_across_feeds_renders_identically() {
    // 日 = U+65E5 = 0xE6 0x97 0xA5 in UTF-8 (3 bytes, wide cell).
    let bytes = [0xE6, 0x97, 0xA5, b'X'];

    let atomic = AlacrittyEngine::new(20, 4);
    atomic.feed(&bytes).unwrap();
    let h_atomic = atomic.snapshot().canonical_hash();

    // Split between every adjacent pair of bytes — buffer must stitch.
    for split in 1..bytes.len() {
        let e = AlacrittyEngine::new(20, 4);
        e.feed(&bytes[..split]).unwrap();
        e.feed(&bytes[split..]).unwrap();
        assert_eq!(
            e.snapshot().canonical_hash(),
            h_atomic,
            "split at {split} produced a different hash than atomic feed"
        );
    }
}

/// Regression test for a quirk found driving GNU nano in an 80x24 PTY:
/// nano writes its right-aligned `Modified` flag with cursor positioning
/// that lands the first char at column 71 and the rest at 72..78, but
/// only `M` at col 79 actually shows up in the engine's grid.
///
/// This narrow ANSI sequence reproduces the same pattern: position the
/// cursor at row 0 col 71 via CUP, then write "Modified". Engine grid
/// should hold all 8 chars; if anything between col 71-78 is lost it's
/// the same bug.
#[test]
fn cup_then_write_at_high_columns_preserves_all_chars() {
    let eng = AlacrittyEngine::new(80, 24);
    // CUP row 1 col 72 (1-indexed). Then write 'Modified'.
    eng.feed(b"\x1b[1;72HModified").expect("feed ok");
    let snap = eng.snapshot();
    let row0: String = (0..usize::from(snap.grid.cols))
        .map(|col| snap.grid.cells[col].ch.as_str())
        .collect::<String>();
    let row0_trim = row0.trim_end();
    assert!(
        row0_trim.ends_with("Modified"),
        "row 0 should end with 'Modified'; got {row0_trim:?}"
    );
}

/// Stress: feed nano's "Modified" byte stream split into 1-byte chunks.
/// If any state machine drops bytes across feed boundaries (CSI mid-
/// parse, SCS mid-parse, etc.), the assertion will catch it.
#[test]
fn replay_nano_modified_byte_stream_under_one_byte_chunks() {
    let bytes = include_bytes!("nano-modified.bin");
    let eng = AlacrittyEngine::new(80, 24);
    for b in bytes {
        eng.feed(std::slice::from_ref(b)).expect("feed ok");
    }
    let snap = eng.snapshot();
    let row0: String = (0..usize::from(snap.grid.cols))
        .map(|col| snap.grid.cells[col].ch.as_str())
        .collect::<String>();
    assert!(
        row0.contains("Modified"),
        "row 0 should contain 'Modified' after one-byte-at-a-time replay; got {row0:?}"
    );
}

/// Regression replay of GNU nano's exact byte stream when writing its
/// `Modified` flag, captured from a real test run.
///
/// nano's sequence is `CUP(1,71) → SCS G0=ASCII → SGR(0;7) → "Modified"`.
/// When fed all at once through the engine, the literal text "Modified"
/// must land in the cell grid in full — not truncated to "M" at the
/// last column, which was the original failure mode found in
/// `bwrap_nano_typed_buffer_shows_modified`.
///
/// The bytes live in `nano-modified.bin` so the test stays fast (no
/// PTY, no bwrap, no nano binary needed in the engine crate).
#[test]
fn replay_nano_modified_byte_stream_shows_modified() {
    let bytes = include_bytes!("nano-modified.bin");
    let eng = AlacrittyEngine::new(80, 24);
    eng.feed(bytes).expect("feed ok");
    let snap = eng.snapshot();
    let row0: String = (0..usize::from(snap.grid.cols))
        .map(|col| snap.grid.cells[col].ch.as_str())
        .collect::<String>();
    assert!(
        row0.contains("Modified"),
        "row 0 should contain 'Modified' after replaying nano's bytes; got {row0:?}"
    );
}

#[tokio::test]
async fn resize_updates_dimensions_and_emits_event() {
    let eng = AlacrittyEngine::new(80, 24);
    let mut sub = eng.subscribe();
    eng.resize(132, 40).expect("resize ok");
    let evt = sub.recv().await.expect("event arrives");
    assert_eq!(evt.kind, MutationKind::Resize);
    let snap = eng.snapshot();
    assert_eq!(snap.grid.cols, 132);
    assert_eq!(snap.grid.rows, 40);
    assert_eq!(snap.grid.cells.len(), 132 * 40, "cells len = cols*rows");
}
