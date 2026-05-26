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
