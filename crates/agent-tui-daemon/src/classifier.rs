//! 9-state pane classifier (RFC §6.2).
//!
//! Heuristics over the visible cell grid + engine modes. The classifier is
//! deliberately conservative: any ambiguous state degrades to
//! `PaneState::Unknown` rather than misreport. Per-program adapters (P2)
//! refine the classification; for v1 we only have the generic adapter.

use agent_tui_engine::EngineSnapshot;
use agent_tui_protocol::PaneState;
use regex::Regex;
use std::sync::OnceLock;

/// Classify a pane snapshot.
#[must_use]
pub fn classify(snap: &EngineSnapshot) -> PaneState {
    if snap.modes.alt_screen {
        return PaneState::AltScreenTui;
    }
    let body = visible_text(snap);
    let last_line = last_non_empty_line(&body);

    if password_re().is_match(last_line) {
        return PaneState::PasswordPrompt;
    }
    if confirm_re().is_match(last_line) {
        return PaneState::Confirm;
    }
    if pager_re().is_match(last_line) {
        return PaneState::Pager;
    }
    if repl_re().is_match(last_line) {
        return PaneState::Repl;
    }
    PaneState::Unknown
}

fn visible_text(snap: &EngineSnapshot) -> String {
    let cols = usize::from(snap.grid.cols);
    let rows = usize::from(snap.grid.rows);
    let mut out = String::with_capacity(cols * rows);
    for row in 0..rows {
        for col in 0..cols {
            let cell = &snap.grid.cells[row * cols + col];
            if cell.width == 0 {
                continue;
            }
            out.push_str(&cell.ch);
        }
        out.push('\n');
    }
    out
}

fn last_non_empty_line(body: &str) -> &str {
    body.lines()
        .map(str::trim_end)
        .rfind(|l| !l.is_empty())
        .unwrap_or("")
}

fn password_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)(password|passphrase)\s*:\s*$").unwrap())
}

fn confirm_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // [Y/n], (Y/n), [y/N], Continue? — common confirmation tails.
        Regex::new(r"(?i)(\[[yn]/[yn]\]|\([yn]/[yn]\)|continue\?|are you sure\??)\s*$").unwrap()
    })
}

fn pager_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)(--more--|^\(END\)$|^:$|lines? \d+(-\d+)?/\d+)").unwrap())
}

fn repl_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // Common interactive REPL prompts. Conservative — avoid false-positives
        // on shell prompts (which generally end with $, # or %).
        Regex::new(r"^(>>>|In \[\d+\]:|irb\([^)]*\)>|>\s*$)").unwrap()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_tui_engine::{Cell, CellGrid, EngineSnapshot, ModeFlags};

    fn snap_with(content: &str, alt_screen: bool) -> EngineSnapshot {
        let cols: u16 = 40;
        let rows: u16 = 6;
        let total = usize::from(cols) * usize::from(rows);
        let mut cells = vec![
            Cell {
                ch: " ".into(),
                width: 1,
                fg: 0,
                bg: 0,
                attrs: 0,
            };
            total
        ];
        let mut row = 0usize;
        let mut col = 0usize;
        for ch in content.chars() {
            if ch == '\n' {
                row += 1;
                col = 0;
                continue;
            }
            if row >= usize::from(rows) {
                break;
            }
            cells[row * usize::from(cols) + col] = Cell {
                ch: ch.to_string(),
                width: 1,
                fg: 0,
                bg: 0,
                attrs: 0,
            };
            col += 1;
            if col >= usize::from(cols) {
                row += 1;
                col = 0;
            }
        }
        EngineSnapshot {
            grid: CellGrid {
                cols,
                rows,
                cells,
                cursor: (0, 0),
            },
            modes: ModeFlags {
                alt_screen,
                ..ModeFlags::default()
            },
            sequence: 1,
        }
    }

    #[test]
    fn alt_screen_classifies_as_tui() {
        assert_eq!(classify(&snap_with("", true)), PaneState::AltScreenTui);
    }

    #[test]
    fn password_prompt_recognized() {
        assert_eq!(
            classify(&snap_with("Password:", false)),
            PaneState::PasswordPrompt
        );
        assert_eq!(
            classify(&snap_with("Enter passphrase: ", false)),
            PaneState::PasswordPrompt
        );
    }

    #[test]
    fn yn_confirm_recognized() {
        assert_eq!(
            classify(&snap_with("Proceed? [Y/n]", false)),
            PaneState::Confirm
        );
        assert_eq!(classify(&snap_with("Continue?", false)), PaneState::Confirm);
    }

    #[test]
    fn pager_footer_recognized() {
        assert_eq!(
            classify(&snap_with("lines 1-24/200", false)),
            PaneState::Pager
        );
        assert_eq!(classify(&snap_with("--More--", false)), PaneState::Pager);
        assert_eq!(classify(&snap_with(":", false)), PaneState::Pager);
    }

    #[test]
    fn python_repl_prompt_recognized() {
        assert_eq!(classify(&snap_with(">>>", false)), PaneState::Repl);
    }

    #[test]
    fn plain_text_is_unknown() {
        assert_eq!(
            classify(&snap_with("some shell output here", false)),
            PaneState::Unknown
        );
    }
}
