//! Regression: PTY winsize env normalization.
//!
//! `PtyChild::spawn` must set `LINES` and `COLUMNS` in the child's env
//! to match the PTY size it just allocated — even when the parent
//! process (the daemon, in deployment) has stale values inherited from
//! the user's outer shell.
//!
//! Why this matters: ncurses-based programs (tig, mc, dialog, …) read
//! `LINES`/`COLUMNS` from env before falling back to `TIOCGWINSZ`.
//! Without the override, ncurses sees the outer shell's dimensions
//! (commonly 50+ rows on modern monitors) and draws into virtual rows
//! past the real grid, leaving the visible pane mostly blank with
//! chrome displaced to the bottom rows.
//!
//! See `crates/agent-tui-daemon/src/pty.rs` (`PtyChild::spawn`) for the
//! fix point.

#![cfg(unix)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_tui_daemon::pty::PtyChild;
use agent_tui_engine::Engine;
use agent_tui_engine_alacritty::AlacrittyEngine;

/// Wait up to `timeout` for the engine grid to contain `needle`.
fn wait_for_text(eng: &dyn Engine, needle: &str, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let snap = eng.snapshot();
        let cols = usize::from(snap.grid.cols);
        let mut buf = String::with_capacity(snap.grid.cells.len());
        for c in &snap.grid.cells {
            buf.push_str(&c.ch);
        }
        // De-pad each row for readable matching.
        let mut joined = String::new();
        for row in buf.as_bytes().chunks(cols) {
            joined.push_str(std::str::from_utf8(row).unwrap_or(""));
            joined.push('\n');
        }
        if joined.contains(needle) {
            return Some(joined);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

#[tokio::test(flavor = "current_thread")]
async fn pty_spawn_sets_lines_columns_to_pty_size() {
    // Pollute the process env with the outer-shell dimensions that would
    // otherwise leak through. This mirrors real deployment: agent-tui's
    // CLI inherits LINES/COLUMNS from the user's shell, propagates to
    // the daemon, then portable-pty's CommandBuilder inherits them.
    // SAFETY: single-threaded test; no other threads read these vars.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("LINES", "999");
        std::env::set_var("COLUMNS", "999");
    }

    let cols = 80u16;
    let rows = 24u16;
    let engine: Arc<dyn Engine> = Arc::new(AlacrittyEngine::new(cols, rows));
    let argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        // Echo the env vars the child sees + the TIOCGWINSZ-reported
        // size so we can compare. `stty size` prints "rows cols".
        // Print, then exit. PtyChild::Drop kills any straggler; we
        // don't need the child to outlive the read. A short `sleep`
        // keeps the PTY readable while we drain the engine.
        "printf 'L=%s C=%s S=%s\\n' \"$LINES\" \"$COLUMNS\" \"$(stty size)\"; sleep 1".to_string(),
    ];
    let _pty = PtyChild::spawn(&argv, None, cols, rows, engine.clone(), None)
        .expect("spawn child under PTY");

    // The child's printf line should land in the engine grid within a
    // second on any reasonable machine.
    let captured =
        wait_for_text(&*engine, "L=", Duration::from_secs(3)).expect("child printf reached engine");

    // Both env vars MUST match the PTY size, regardless of what the
    // parent had set. Also: `stty size` reports the same numbers from
    // TIOCGWINSZ, proving the PTY is actually that size end-to-end.
    assert!(
        captured.contains("L=24"),
        "LINES not normalized to PTY rows; got\n---\n{captured}\n---"
    );
    assert!(
        captured.contains("C=80"),
        "COLUMNS not normalized to PTY cols; got\n---\n{captured}\n---"
    );
    assert!(
        captured.contains("S=24 80"),
        "TIOCGWINSZ disagrees with the PTY allocation; got\n---\n{captured}\n---"
    );

    #[allow(unsafe_code)]
    unsafe {
        std::env::remove_var("LINES");
        std::env::remove_var("COLUMNS");
    }
}
