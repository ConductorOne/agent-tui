//! OSC 133 (`FinalTerm` shell-integration) byte-stream parser.
//!
//! Scans raw PTY bytes for `ESC ] 133 ; <kind> [; <payload>] ST` sequences
//! and yields the recognized marker kind. `ST` is either `BEL` (0x07) or
//! `ESC \` (0x1B 0x5C). We don't validate the payload past the leading
//! kind byte — that's enough to drive the shell-state classifier.
//!
//! Used by `PtyChild::pty_reader_loop` so the daemon can know "this pane is
//! showing a shell prompt" even when the engine doesn't expose OSC 133
//! events natively. RFC §6.2 — `shell` state detection.

/// Recognized OSC 133 marker kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// `A` — prompt start (shell is about to draw its prompt).
    PromptStart,
    /// `B` — command start (user input region begins).
    CommandStart,
    /// `C` — command output starts (foreground command running).
    OutputStart,
    /// `D[;exit]` — command end. The optional exit code is discarded today.
    CommandEnd,
}

/// Incremental scanner state. One per pane; lives in `PtyChild`.
#[derive(Debug, Default)]
pub struct Scanner {
    state: State,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Looking for `ESC` (0x1B).
    #[default]
    Idle,
    /// Saw `ESC`, looking for `]` (0x5D).
    Esc,
    /// Saw `ESC ]`, accumulating the OSC prefix `133;`.
    Osc { matched: u8 },
    /// Saw `ESC ] 133 ;` — next byte is the kind letter.
    AwaitKind,
    /// Got the kind; consuming any payload until ST.
    InPayload { kind: Marker },
    /// In a possible `ESC \` ST.
    EscInPayload { kind: Marker },
}

const PREFIX: &[u8] = b"133;";

impl Scanner {
    /// Construct an idle scanner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; returns every complete marker seen in this chunk.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Marker> {
        let mut out = Vec::new();
        for &b in bytes {
            self.step(b, &mut out);
        }
        out
    }

    fn step(&mut self, b: u8, out: &mut Vec<Marker>) {
        match self.state {
            State::Idle => {
                if b == 0x1B {
                    self.state = State::Esc;
                }
            }
            State::Esc => {
                self.state = if b == b']' {
                    State::Osc { matched: 0 }
                } else if b == 0x1B {
                    State::Esc
                } else {
                    State::Idle
                };
            }
            State::Osc { matched } => {
                if usize::from(matched) < PREFIX.len() && b == PREFIX[usize::from(matched)] {
                    let n = matched + 1;
                    self.state = if usize::from(n) == PREFIX.len() {
                        State::AwaitKind
                    } else {
                        State::Osc { matched: n }
                    };
                } else {
                    self.state = State::Idle;
                }
            }
            State::AwaitKind => {
                let kind = match b {
                    b'A' => Some(Marker::PromptStart),
                    b'B' => Some(Marker::CommandStart),
                    b'C' => Some(Marker::OutputStart),
                    b'D' => Some(Marker::CommandEnd),
                    _ => None,
                };
                self.state = if let Some(k) = kind {
                    State::InPayload { kind: k }
                } else {
                    State::Idle
                };
            }
            State::InPayload { kind } => {
                self.state = match b {
                    0x07 => {
                        // BEL ST.
                        out.push(kind);
                        State::Idle
                    }
                    0x1B => State::EscInPayload { kind },
                    _ => State::InPayload { kind },
                };
            }
            State::EscInPayload { kind } => {
                self.state = if b == b'\\' {
                    // ESC \ ST.
                    out.push(kind);
                    State::Idle
                } else {
                    State::InPayload { kind }
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bel_terminated_a() {
        let mut s = Scanner::new();
        let m = s.feed(b"\x1b]133;A\x07");
        assert_eq!(m, vec![Marker::PromptStart]);
    }

    #[test]
    fn parses_st_terminated_c() {
        let mut s = Scanner::new();
        let m = s.feed(b"\x1b]133;C\x1b\\");
        assert_eq!(m, vec![Marker::OutputStart]);
    }

    #[test]
    fn parses_d_with_exit_payload() {
        let mut s = Scanner::new();
        let m = s.feed(b"\x1b]133;D;0\x07");
        assert_eq!(m, vec![Marker::CommandEnd]);
    }

    #[test]
    fn chunked_input_round_trips() {
        let mut s = Scanner::new();
        assert_eq!(s.feed(b"\x1b]133"), vec![]);
        assert_eq!(s.feed(b";B"), vec![]);
        assert_eq!(s.feed(b"\x07"), vec![Marker::CommandStart]);
    }

    #[test]
    fn ignores_unrelated_escapes() {
        let mut s = Scanner::new();
        let m = s.feed(b"\x1b[31mred\x1b[0m\x1b]133;A\x07");
        assert_eq!(m, vec![Marker::PromptStart]);
    }

    #[test]
    fn ignores_unknown_kind() {
        let mut s = Scanner::new();
        let m = s.feed(b"\x1b]133;Z\x07");
        assert!(m.is_empty());
    }

    // -------------------------------------------------------------------
    // Property tests. Streaming byte-state-machines are bug-prone in the
    // chunking and reset corners — proptest is exactly the right tool.
    // -------------------------------------------------------------------
    mod proptests {
        use super::{Marker, Scanner};
        use proptest::prelude::*;

        /// Atomic feed: feed the whole buffer in one call.
        fn run_atomic(bytes: &[u8]) -> Vec<Marker> {
            Scanner::new().feed(bytes).clone()
        }

        /// Chunked feed: feed `chunk_sizes`-shaped pieces in sequence.
        /// Sizes are normalized to fit the buffer; remainder goes in a
        /// final chunk so we always feed the entire input exactly once.
        fn run_chunked(bytes: &[u8], chunk_sizes: &[usize]) -> Vec<Marker> {
            let mut s = Scanner::new();
            let mut out = Vec::new();
            let mut idx = 0;
            for &cs in chunk_sizes {
                if idx >= bytes.len() {
                    break;
                }
                let n = cs.clamp(1, bytes.len() - idx);
                out.extend(s.feed(&bytes[idx..idx + n]));
                idx += n;
            }
            if idx < bytes.len() {
                out.extend(s.feed(&bytes[idx..]));
            }
            out
        }

        /// Byte-by-byte feed: the most adversarial chunking — one byte per
        /// `feed` call. This exposes any state machine that "looks ahead"
        /// across calls without storing partial state.
        fn run_one_at_a_time(bytes: &[u8]) -> Vec<Marker> {
            let mut s = Scanner::new();
            let mut out = Vec::new();
            for b in bytes {
                out.extend(s.feed(std::slice::from_ref(b)));
            }
            out
        }

        proptest! {
            /// Feeding the same input atomically vs in arbitrary chunks
            /// must yield the same marker stream. This is THE property a
            /// streaming scanner has to satisfy.
            #[test]
            fn chunked_equiv_atomic(
                bytes in prop::collection::vec(any::<u8>(), 0..256),
                chunks in prop::collection::vec(1usize..32, 0..32),
            ) {
                let atomic = run_atomic(&bytes);
                let chunked = run_chunked(&bytes, &chunks);
                prop_assert_eq!(atomic, chunked);
            }

            /// Even one-byte-at-a-time matches atomic feed. Stronger
            /// statement than `chunked_equiv_atomic`: no state machine
            /// shortcut crosses input boundaries.
            #[test]
            fn one_byte_at_a_time_equiv_atomic(
                bytes in prop::collection::vec(any::<u8>(), 0..256),
            ) {
                let atomic = run_atomic(&bytes);
                let stepped = run_one_at_a_time(&bytes);
                prop_assert_eq!(atomic, stepped);
            }

            /// `feed` is total — for any input, the scanner returns a
            /// Vec without panicking. Catches indexing bugs in the state
            /// machine and any future regression that adds an `unwrap`.
            #[test]
            fn feed_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
                let _ = Scanner::new().feed(&bytes);
            }

            /// Number of markers emitted is bounded by `len(input) / 7`
            /// — the shortest valid OSC 133 sequence is `ESC ] 1 3 3 ;
            /// kind BEL` = 7 bytes. A stronger bound rules out a "single
            /// byte produces many markers" regression class.
            #[test]
            fn marker_count_bounded(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
                let markers = run_atomic(&bytes);
                prop_assert!(
                    markers.len() <= bytes.len() / 7 + 1,
                    "got {} markers from {} bytes",
                    markers.len(), bytes.len()
                );
            }

            /// Wrapping a valid OSC 133 sequence in random garbage on
            /// either side must still yield exactly one marker. This is
            /// the practical case: real PTY output is OSC 133 markers
            /// interleaved with ANSI color codes, text, repaints.
            #[test]
            fn valid_marker_survives_noise(
                prefix in prop::collection::vec(any::<u8>(), 0..32),
                suffix in prop::collection::vec(any::<u8>(), 0..32),
                kind in prop_oneof![Just(b'A'), Just(b'B'), Just(b'C'), Just(b'D')],
            ) {
                let mut buf = prefix.clone();
                // Filter out 0x1B from prefix to avoid the prefix
                // accidentally starting another OSC sequence that consumes
                // our marker. (Real-world: 0x1B is rare in ANSI noise, so
                // this just isolates the property we want to assert.)
                buf.retain(|b| *b != 0x1B);
                buf.extend_from_slice(&[0x1B, b']', b'1', b'3', b'3', b';', kind, 0x07]);
                buf.extend_from_slice(&suffix);

                let markers = run_atomic(&buf);
                let expected = match kind {
                    b'A' => Marker::PromptStart,
                    b'B' => Marker::CommandStart,
                    b'C' => Marker::OutputStart,
                    b'D' => Marker::CommandEnd,
                    _ => unreachable!(),
                };
                prop_assert!(
                    markers.contains(&expected),
                    "expected {:?} in markers {:?}", expected, markers
                );
            }
        }
    }
}
