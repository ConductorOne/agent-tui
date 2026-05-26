//! Property tests for `AlacrittyEngine`.
//!
//! High-value invariants the engine must satisfy under any byte sequence:
//!  - **Determinism**: two engines fed identical bytes hash equal.
//!  - **Associativity**: feeding `a||b` in one call hashes the same as
//!    feeding `a` then `b` — proves no parser state is lost across calls.
//!  - **Totality**: `feed` never panics on arbitrary bytes.
//!  - **Grid invariants**: `cells.len() == cols*rows`, cursor always
//!    within bounds.

use agent_tui_engine::Engine;
use agent_tui_engine_alacritty::AlacrittyEngine;
use proptest::prelude::*;

fn feed_and_hash(cols: u16, rows: u16, bytes: &[u8]) -> String {
    let e = AlacrittyEngine::new(cols, rows);
    e.feed(bytes).expect("feed ok");
    e.snapshot().canonical_hash()
}

fn feed_chunked_and_hash(cols: u16, rows: u16, chunks: &[&[u8]]) -> String {
    let e = AlacrittyEngine::new(cols, rows);
    for c in chunks {
        e.feed(c).expect("feed ok");
    }
    e.snapshot().canonical_hash()
}

proptest! {
    /// Two engines initialized identically and fed the same bytes
    /// produce identical canonical hashes. Foundation for the wait
    /// subsystem (`wait --hash`).
    #[test]
    fn determinism(
        bytes in prop::collection::vec(any::<u8>(), 0..512),
        cols in 8u16..120,
        rows in 4u16..40,
    ) {
        let h1 = feed_and_hash(cols, rows, &bytes);
        let h2 = feed_and_hash(cols, rows, &bytes);
        prop_assert_eq!(h1, h2);
    }

    /// Feeding `a || b` atomically equals feeding `a` then `b`. Catches
    /// any parser-state-leak between feed calls.
    ///
    /// Inputs are constrained to byte ranges that don't form multi-byte
    /// UTF-8 sequences (so chunk boundaries never split a codepoint).
    /// The proptest with the full byte range exposes an upstream
    /// alacritty_terminal bug where `Processor::advance` doesn't buffer
    /// incomplete UTF-8 between calls — fixed by `AlacrittyEngine`'s
    /// own UTF-8 buffer (see `lib.rs`). The buffer's correctness has
    /// dedicated tests below.
    #[test]
    fn feed_is_associative(
        a in prop::collection::vec(0x20u8..=0x7E, 0..256),
        b in prop::collection::vec(0x20u8..=0x7E, 0..256),
    ) {
        let mut both = a.clone();
        both.extend_from_slice(&b);
        let atomic = feed_and_hash(40, 12, &both);
        let split = feed_chunked_and_hash(40, 12, &[&a, &b]);
        prop_assert_eq!(atomic, split);
    }

    /// Feeding the same total bytes broken into ANY number of arbitrary
    /// chunks hashes equal to atomic feed. Constrained to printable
    /// ASCII for the same UTF-8 boundary reason as `feed_is_associative`.
    #[test]
    fn feed_is_associative_under_any_chunking(
        bytes in prop::collection::vec(0x20u8..=0x7E, 0..512),
        split_at in prop::collection::vec(0usize..512, 0..8),
    ) {
        if bytes.is_empty() {
            return Ok(());
        }
        let atomic = feed_and_hash(40, 12, &bytes);

        // Build a strictly-increasing list of split offsets within range.
        let mut offsets: Vec<usize> = split_at
            .into_iter()
            .map(|n| n % bytes.len())
            .collect();
        offsets.sort_unstable();
        offsets.dedup();
        let mut chunks: Vec<&[u8]> = Vec::new();
        let mut prev = 0;
        for o in &offsets {
            chunks.push(&bytes[prev..*o]);
            prev = *o;
        }
        chunks.push(&bytes[prev..]);
        let chunked = feed_chunked_and_hash(40, 12, &chunks);
        prop_assert_eq!(atomic, chunked);
    }

    /// Associativity over *valid UTF-8* inputs, including multi-byte
    /// sequences, when the engine handles chunk boundaries correctly.
    /// Bytes are reconstructed from random `char`s and split anywhere —
    /// the engine's UTF-8 buffer is responsible for stitching incomplete
    /// codepoints across feed boundaries.
    #[test]
    fn feed_is_associative_for_utf8(
        text in proptest::string::string_regex("[\\PC]{0,128}").unwrap(),
        split_at in prop::collection::vec(0usize..256, 0..8),
    ) {
        let bytes = text.as_bytes();
        if bytes.is_empty() {
            return Ok(());
        }
        let atomic = feed_and_hash(40, 12, bytes);

        let mut offsets: Vec<usize> = split_at
            .into_iter()
            .map(|n| n % bytes.len())
            .collect();
        offsets.sort_unstable();
        offsets.dedup();
        let mut chunks: Vec<&[u8]> = Vec::new();
        let mut prev = 0;
        for o in &offsets {
            chunks.push(&bytes[prev..*o]);
            prev = *o;
        }
        chunks.push(&bytes[prev..]);
        let chunked = feed_chunked_and_hash(40, 12, &chunks);
        prop_assert_eq!(atomic, chunked);
    }

    /// `feed` never panics. Catches indexing / `unwrap` regressions in
    /// the engine's byte-handling path.
    #[test]
    fn feed_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let e = AlacrittyEngine::new(40, 12);
        // Result is allowed to be Err — we just want no panic.
        let _ = e.feed(&bytes);
    }

    /// Grid invariants hold after ANY feed: cells.len() == cols*rows,
    /// cursor is within `[0, rows) × [0, cols)`. These are the contracts
    /// the snapshot consumers rely on.
    #[test]
    fn grid_invariants_hold(
        bytes in prop::collection::vec(any::<u8>(), 0..512),
        cols in 8u16..120,
        rows in 4u16..40,
    ) {
        let e = AlacrittyEngine::new(cols, rows);
        let _ = e.feed(&bytes);
        let snap = e.snapshot();
        prop_assert_eq!(
            snap.grid.cells.len(),
            usize::from(snap.grid.cols) * usize::from(snap.grid.rows),
            "cells.len() must equal cols*rows"
        );
        let (cy, cx) = snap.grid.cursor;
        prop_assert!(cy < snap.grid.rows, "cursor row {} >= rows {}", cy, snap.grid.rows);
        prop_assert!(cx < snap.grid.cols, "cursor col {} >= cols {}", cx, snap.grid.cols);
    }

    /// Resize maintains grid invariants. Catches resize implementations
    /// that forget to clamp the cursor or reallocate the cell buffer.
    #[test]
    fn resize_preserves_grid_invariants(
        bytes in prop::collection::vec(any::<u8>(), 0..256),
        cols_a in 8u16..80, rows_a in 4u16..30,
        cols_b in 8u16..80, rows_b in 4u16..30,
    ) {
        let e = AlacrittyEngine::new(cols_a, rows_a);
        let _ = e.feed(&bytes);
        e.resize(cols_b, rows_b).expect("resize ok");
        let snap = e.snapshot();
        prop_assert_eq!(snap.grid.cols, cols_b);
        prop_assert_eq!(snap.grid.rows, rows_b);
        prop_assert_eq!(
            snap.grid.cells.len(),
            usize::from(cols_b) * usize::from(rows_b)
        );
        let (cy, cx) = snap.grid.cursor;
        prop_assert!(cy < rows_b, "cursor row {} out of bounds after resize", cy);
        prop_assert!(cx < cols_b, "cursor col {} out of bounds after resize", cx);
    }

    /// Sequence number is strictly monotonic across feeds. The wait
    /// subsystem depends on this — a duplicate or backwards sequence
    /// would let a stale "wait until N+1" satisfy itself spuriously.
    #[test]
    fn sequence_is_monotonic(
        feeds in prop::collection::vec(
            prop::collection::vec(any::<u8>(), 1..64),
            1..16,
        ),
    ) {
        let e = AlacrittyEngine::new(40, 12);
        let mut last = e.snapshot().sequence;
        for chunk in &feeds {
            e.feed(chunk).expect("feed ok");
            let cur = e.snapshot().sequence;
            prop_assert!(cur > last, "sequence went {} -> {} (not strictly increasing)", last, cur);
            last = cur;
        }
    }
}
