//! Built-in adapters shipped with the daemon.
//!
//! v1 ships three: `generic` (sectioned outline fallback), `claude-code`
//! (pattern-matcher for the Ink/Claude family of CLI agents), and `shell`
//! (last-line prompt heuristic + argv hint). nvim and tmux adapters live
//! in this module too once the live-RPC plumbing is ready (P3).

#![allow(clippy::cast_possible_truncation)]

use std::collections::BTreeSet;

use agent_tui_engine::EngineSnapshot;
use agent_tui_protocol::{Outline, OutlineNode};

use crate::{Adapter, AdapterError, Capabilities, PaneInfo};

/// Generic-purpose fallback adapter.
///
/// Produces a coarsely-sectioned outline (header / body / footer) by scanning
/// the visible cell grid. Detect always returns 0.1 — every other adapter
/// outscores it when it can.
pub struct GenericAdapter;

#[async_trait::async_trait]
impl Adapter for GenericAdapter {
    fn name(&self) -> &'static str {
        "generic"
    }

    async fn detect(&self, _info: &PaneInfo) -> f32 {
        0.1
    }

    async fn initialize(&self) -> Result<Capabilities, AdapterError> {
        Ok(default_caps())
    }

    async fn outline(&self, snap: &EngineSnapshot) -> Result<Outline, AdapterError> {
        let rows = grid_rows(snap);
        let header = first_non_empty(&rows);
        let footer = last_non_empty(&rows);
        let body = body_lines(&rows, header, footer);

        let mut nodes = Vec::new();
        let mut next_idx = 1u32;
        if let Some(row_idx) = header {
            nodes.push(OutlineNode {
                r#ref: format!("@e{next_idx}"),
                role: "header".into(),
                name: rows[row_idx].clone(),
                value: None,
                focused: false,
                anchor: Some((row_idx as u16, 0)),
                children: Vec::new(),
            });
            next_idx += 1;
        }
        if !body.is_empty() {
            nodes.push(OutlineNode {
                r#ref: format!("@e{next_idx}"),
                role: "buffer".into(),
                name: body,
                value: None,
                focused: true,
                anchor: Some((header.map_or(0, |r| (r + 1) as u16), 0)),
                children: Vec::new(),
            });
            next_idx += 1;
        }
        if let Some(row_idx) = footer
            && Some(row_idx) != header
        {
            nodes.push(OutlineNode {
                r#ref: format!("@e{next_idx}"),
                role: "footer".into(),
                name: rows[row_idx].clone(),
                value: None,
                focused: false,
                anchor: Some((row_idx as u16, 0)),
                children: Vec::new(),
            });
        }

        Ok(Outline {
            adapter: "generic".into(),
            nodes,
        })
    }

    async fn eval(&self, _expr: &str) -> Result<serde_json::Value, AdapterError> {
        Err(AdapterError::Refused(
            "generic adapter does not support eval".into(),
        ))
    }

    async fn shutdown(&self) -> Result<(), AdapterError> {
        Ok(())
    }
}

/// claude-code adapter: pattern matcher for the Claude/Codex/Aider family of
/// CLI agents. v1 detection is argv-based; first-bytes Ink-banner detection
/// lands when the spawn handler starts buffering early output.
pub struct ClaudeCodeAdapter;

const CLAUDE_LIKE_BINS: &[&str] = &["claude", "claude-code", "codex", "aider", "opencode"];

#[async_trait::async_trait]
impl Adapter for ClaudeCodeAdapter {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    async fn detect(&self, info: &PaneInfo) -> f32 {
        if CLAUDE_LIKE_BINS.contains(&info.comm.as_str()) {
            return 0.9;
        }
        // Argv-substring fallback for wrapper scripts.
        if info
            .argv
            .iter()
            .any(|a| CLAUDE_LIKE_BINS.iter().any(|n| a.contains(n)))
        {
            return 0.6;
        }
        0.0
    }

    async fn initialize(&self) -> Result<Capabilities, AdapterError> {
        Ok(default_caps())
    }

    async fn outline(&self, snap: &EngineSnapshot) -> Result<Outline, AdapterError> {
        // Same sectioned shape as generic, but tagged as claude-code so the
        // agent harness knows what's underneath.
        let mut outline = GenericAdapter.outline(snap).await?;
        outline.adapter = "claude-code".into();
        Ok(outline)
    }

    async fn eval(&self, _expr: &str) -> Result<serde_json::Value, AdapterError> {
        Err(AdapterError::Refused(
            "claude-code adapter does not support eval".into(),
        ))
    }

    async fn shutdown(&self) -> Result<(), AdapterError> {
        Ok(())
    }
}

/// Shell adapter: detects POSIX shells by argv and exposes a prompt-aware
/// outline. Drives `PaneState::Shell` via the classifier downstream once the
/// snapshot handler reads the adapter name.
pub struct ShellAdapter;

const SHELL_BINS: &[&str] = &["bash", "zsh", "fish", "sh", "dash", "ksh", "mksh"];

#[async_trait::async_trait]
impl Adapter for ShellAdapter {
    fn name(&self) -> &'static str {
        "shell"
    }

    async fn detect(&self, info: &PaneInfo) -> f32 {
        if SHELL_BINS.contains(&info.comm.as_str()) {
            return 0.85;
        }
        let comm_lower = info.comm.to_ascii_lowercase();
        if SHELL_BINS.iter().any(|s| comm_lower.ends_with(s)) {
            return 0.4;
        }
        0.0
    }

    async fn initialize(&self) -> Result<Capabilities, AdapterError> {
        Ok(default_caps())
    }

    async fn outline(&self, snap: &EngineSnapshot) -> Result<Outline, AdapterError> {
        let rows = grid_rows(snap);
        let prompt = last_non_empty(&rows).map(|i| rows[i].clone());
        let body = rows
            .iter()
            .filter(|r| !r.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");

        let mut nodes = vec![OutlineNode {
            r#ref: "@e1".into(),
            role: "buffer".into(),
            name: body,
            value: None,
            focused: true,
            anchor: Some((0, 0)),
            children: Vec::new(),
        }];
        if let Some(p) = prompt {
            nodes.push(OutlineNode {
                r#ref: "@e2".into(),
                role: "prompt".into(),
                name: p,
                value: None,
                focused: false,
                anchor: None,
                children: Vec::new(),
            });
        }

        Ok(Outline {
            adapter: "shell".into(),
            nodes,
        })
    }

    async fn eval(&self, _expr: &str) -> Result<serde_json::Value, AdapterError> {
        Err(AdapterError::Refused(
            "shell adapter does not support eval".into(),
        ))
    }

    async fn shutdown(&self) -> Result<(), AdapterError> {
        Ok(())
    }
}

/// Vim/Neovim adapter. Detects vim-family editors by argv and exposes a
/// structured outline: mode (normal/insert/visual/command/search/replace),
/// filename, modified flag, and the buffer body separately from the
/// statusline + command-line rows.
///
/// Why this exists: with the generic adapter, an agent driving vim sees
/// a flat block of cells and has to guess from the bytes what mode it's
/// in. With this adapter, `snapshot.outline.nodes` carries `mode=insert`
/// as a top-level field — the agent can read it directly instead of
/// pattern-matching against escape sequences.
///
/// Detection is comm/argv-based today; first-bytes detection (look for
/// alacritty's alt-screen toggle + the `--INSERT--` substring) lands
/// later when the spawn handler starts buffering early output.
pub struct VimAdapter;

const VIM_BINS: &[&str] = &["vim", "nvim", "neovim", "view", "ex", "vimtutor", "vimdiff"];

#[async_trait::async_trait]
impl Adapter for VimAdapter {
    fn name(&self) -> &'static str {
        "vim"
    }

    async fn detect(&self, info: &PaneInfo) -> f32 {
        if VIM_BINS.contains(&info.comm.as_str()) {
            return 0.9;
        }
        // argv basename match — e.g. `["bash", "-c", "/usr/bin/vimtutor"]`.
        if info
            .argv
            .iter()
            .any(|a| VIM_BINS.iter().any(|n| basename_matches(a, n)))
        {
            return 0.7;
        }
        // `bash -c "cp foo bar && vim qux"` wrapper case — substring match
        // against each argv entry. Lower confidence because a passing
        // mention of "vim" in a long command isn't conclusive evidence
        // that vim is the foreground program.
        if info
            .argv
            .iter()
            .any(|a| VIM_BINS.iter().any(|n| word_boundary_contains(a, n)))
        {
            return 0.55;
        }
        0.0
    }

    async fn initialize(&self) -> Result<Capabilities, AdapterError> {
        Ok(default_caps())
    }

    async fn outline(&self, snap: &EngineSnapshot) -> Result<Outline, AdapterError> {
        let rows = grid_rows(snap);
        let parsed = parse_vim_state(&rows);

        // Anchor of the buffer body: from row 0 to whichever row is the
        // first chrome row (statusline or command-line, whichever is
        // higher up).
        let body_end = parsed
            .statusline_row
            .or(parsed.commandline_row)
            .unwrap_or(rows.len());
        let body = rows
            .iter()
            .take(body_end)
            .filter(|r| !r.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");

        let mut nodes = Vec::new();
        let mut next_idx = 1u32;

        nodes.push(OutlineNode {
            r#ref: format!("@e{next_idx}"),
            role: "mode".into(),
            name: parsed.mode.to_string(),
            value: parsed.command_line.clone(),
            focused: false,
            anchor: parsed.commandline_row.map(|r| (r as u16, 0)),
            children: Vec::new(),
        });
        next_idx += 1;

        if let Some(file) = parsed.filename {
            nodes.push(OutlineNode {
                r#ref: format!("@e{next_idx}"),
                role: "file".into(),
                name: file,
                value: parsed.modified.then(|| "modified".to_string()),
                focused: false,
                anchor: parsed.statusline_row.map(|r| (r as u16, 0)),
                children: Vec::new(),
            });
            next_idx += 1;
        }

        if let Some(row) = parsed.statusline_row {
            nodes.push(OutlineNode {
                r#ref: format!("@e{next_idx}"),
                role: "status".into(),
                name: rows[row].clone(),
                value: None,
                focused: false,
                anchor: Some((row as u16, 0)),
                children: Vec::new(),
            });
            next_idx += 1;
        }

        if !body.is_empty() {
            nodes.push(OutlineNode {
                r#ref: format!("@e{next_idx}"),
                role: "buffer".into(),
                name: body,
                value: None,
                focused: true,
                anchor: Some((0, 0)),
                children: Vec::new(),
            });
        }

        Ok(Outline {
            adapter: "vim".into(),
            nodes,
        })
    }

    async fn eval(&self, _expr: &str) -> Result<serde_json::Value, AdapterError> {
        // Real eval would speak msgpack-RPC to `nvim --listen` or vim's
        // `+server2client`. Lands when the JSON-RPC plugin path is wired.
        Err(AdapterError::Refused(
            "vim adapter eval not yet implemented".into(),
        ))
    }

    async fn shutdown(&self) -> Result<(), AdapterError> {
        Ok(())
    }
}

/// Parsed view of vim's chrome rows. None of the fields are mandatory —
/// when vim isn't visible yet (still loading) or the statusline isn't
/// configured, the values stay `None` / `Normal`.
#[derive(Debug, Default)]
struct VimState {
    mode: VimMode,
    /// Row index of the statusline, if found. Statusline is detected as
    /// the second-to-last non-empty row matching a `<file> [N/M]` shape.
    statusline_row: Option<usize>,
    /// Row index of the command-line / mode-indicator row, if active.
    commandline_row: Option<usize>,
    /// Filename extracted from the statusline.
    filename: Option<String>,
    /// `[+]` modified marker present in the statusline.
    modified: bool,
    /// The literal command-line text (e.g. `:w` or `/foo`) when in
    /// command or search mode.
    command_line: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum VimMode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
    VisualBlock,
    Replace,
    Command,
    Search,
    SearchBackward,
}

impl std::fmt::Display for VimMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Normal => "normal",
            Self::Insert => "insert",
            Self::Visual => "visual",
            Self::VisualLine => "visual-line",
            Self::VisualBlock => "visual-block",
            Self::Replace => "replace",
            Self::Command => "command",
            Self::Search => "search",
            Self::SearchBackward => "search-backward",
        })
    }
}

fn parse_vim_state(rows: &[String]) -> VimState {
    let mut state = VimState::default();
    if rows.is_empty() {
        return state;
    }

    // Walk from the bottom looking for vim's chrome rows.
    //
    // Layout with `:set laststatus=2`:
    //   row N-1: statusline (configured statusline text)
    //   row N  : command-line area (empty in normal mode; "-- INSERT --"
    //            in insert; ":foo" in command mode; "/bar" in search)
    //
    // Without laststatus=2, there's no permanent statusline row — only
    // the command-line area.

    let last = rows.len() - 1;
    let last_line = rows[last].trim();

    // Detect mode indicator from the last row.
    state.mode = classify_mode(last_line);
    if state.mode == VimMode::Command
        || state.mode == VimMode::Search
        || state.mode == VimMode::SearchBackward
    {
        state.commandline_row = Some(last);
        state.command_line = Some(last_line.to_string());
    } else if !last_line.is_empty() && is_mode_marker(last_line) {
        state.commandline_row = Some(last);
    }

    // Find the statusline: the highest-indexed row that LOOKS LIKE a
    // statusline (contains a "[N/M]" line/total pattern, or the trailing
    // `[+]` modified marker). Skip the command-line row.
    let statusline_candidate = if last >= 1
        && (state.commandline_row.is_some() || is_statusline_shape(&rows[last - 1]))
    {
        Some(last - 1)
    } else if last_line.contains('[') && last_line.contains(']') {
        Some(last)
    } else {
        None
    };

    if let Some(row) = statusline_candidate
        && is_statusline_shape(&rows[row])
    {
        state.statusline_row = Some(row);
        let (file, modified) = parse_statusline(&rows[row]);
        state.filename = file;
        state.modified = modified;
    }

    state
}

/// `--INSERT--`, `--VISUAL--` etc. are how vim signals mode in the
/// command-line area when `set showmode` is on (the default).
///
/// Real PTY output rarely has the mode marker alone on the row — vim
/// commonly adds trailing context ("W10: Warning: Changing a readonly
/// file", recording-macro indicators, recordings of `:reg` etc.). So
/// we look for the marker as a PREFIX of the trimmed row, not an exact
/// match.
fn classify_mode(text: &str) -> VimMode {
    let trimmed = text.trim();
    if trimmed.starts_with(':') {
        return VimMode::Command;
    }
    if trimmed.starts_with('/') {
        return VimMode::Search;
    }
    if trimmed.starts_with('?') {
        return VimMode::SearchBackward;
    }
    // Mode marker prefixes — match longest-first so VISUAL LINE wins
    // over VISUAL.
    let upper = trimmed.to_ascii_uppercase();
    let markers: &[(&str, VimMode)] = &[
        ("-- VISUAL LINE --", VimMode::VisualLine),
        ("-- VISUAL BLOCK --", VimMode::VisualBlock),
        ("-- V-LINE --", VimMode::VisualLine),
        ("-- V-BLOCK --", VimMode::VisualBlock),
        ("-- INSERT --", VimMode::Insert),
        ("-- REPLACE --", VimMode::Replace),
        ("-- VISUAL --", VimMode::Visual),
        ("(INSERT)", VimMode::Insert),
        ("(REPLACE)", VimMode::Replace),
    ];
    for (marker, mode) in markers {
        if upper.starts_with(marker) {
            return *mode;
        }
    }
    VimMode::Normal
}

fn is_mode_marker(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("--") && trimmed.ends_with("--")
}

/// A statusline matches when it contains `[N/M]` (line/total) or `[+]`
/// (modified marker). The fixture vimrc uses `%f [%l/%L]%m`.
fn is_statusline_shape(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    // The line/total pattern: a bracketed pair of digits separated by `/`.
    let has_line_total = trimmed
        .as_bytes()
        .windows(3)
        .any(|w| w[0] == b'[' && w[1].is_ascii_digit() && (w[2] == b'/' || w[2].is_ascii_digit()));
    let has_modified = trimmed.contains("[+]") || trimmed.contains("[modified]");
    has_line_total || has_modified
}

fn parse_statusline(text: &str) -> (Option<String>, bool) {
    let trimmed = text.trim();
    let modified = trimmed.contains("[+]") || trimmed.contains("[modified]");
    // The fixture statusline format is `%f [%l/%L]%m` -> "filename [N/M]"
    // optionally followed by "[+]". Filename is everything up to the
    // first ' [' boundary, falling back to the first whitespace token.
    let filename = trimmed
        .split_once(" [")
        .map(|(name, _)| name.trim().to_string())
        .or_else(|| {
            trimmed
                .split_whitespace()
                .next()
                .map(std::string::ToString::to_string)
        })
        .filter(|s| !s.is_empty());
    (filename, modified)
}

fn basename_matches(arg: &str, name: &str) -> bool {
    let basename = arg.rsplit('/').next().unwrap_or(arg);
    basename == name
}

/// Does `text` contain `needle` as a whole word? Used for fuzzy argv
/// matching where `needle` ("vim") can appear inside a `bash -c "..."`
/// command string. We require word boundaries on both sides to avoid
/// matching `viminim` or `livening`.
fn word_boundary_contains(text: &str, needle: &str) -> bool {
    let mut start = 0;
    while let Some(off) = text[start..].find(needle) {
        let idx = start + off;
        let before_ok = idx == 0
            || !text.as_bytes()[idx - 1].is_ascii_alphanumeric()
                && text.as_bytes()[idx - 1] != b'_';
        let after_idx = idx + needle.len();
        let after_ok = after_idx == text.len()
            || !text.as_bytes()[after_idx].is_ascii_alphanumeric()
                && text.as_bytes()[after_idx] != b'_';
        if before_ok && after_ok {
            return true;
        }
        start = idx + 1;
    }
    false
}

fn default_caps() -> Capabilities {
    Capabilities {
        supports_eval: false,
        supports_streaming_events: false,
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

fn grid_rows(snap: &EngineSnapshot) -> Vec<String> {
    let cols = usize::from(snap.grid.cols);
    let rows = usize::from(snap.grid.rows);
    let mut out = Vec::with_capacity(rows);
    for row in 0..rows {
        let mut line = String::with_capacity(cols);
        for col in 0..cols {
            let cell = &snap.grid.cells[row * cols + col];
            if cell.width == 0 {
                continue;
            }
            line.push_str(&cell.ch);
        }
        out.push(line.trim_end().to_string());
    }
    out
}

fn first_non_empty(rows: &[String]) -> Option<usize> {
    rows.iter().position(|r| !r.is_empty())
}

fn last_non_empty(rows: &[String]) -> Option<usize> {
    rows.iter().rposition(|r| !r.is_empty())
}

fn body_lines(rows: &[String], header: Option<usize>, footer: Option<usize>) -> String {
    let skip: BTreeSet<usize> = header.into_iter().chain(footer).collect();
    let mut out = String::new();
    let mut first = true;
    for (i, r) in rows.iter().enumerate() {
        if skip.contains(&i) || r.is_empty() {
            continue;
        }
        if !first {
            out.push('\n');
        }
        out.push_str(r);
        first = false;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_tui_engine::{Cell, CellGrid, EngineSnapshot, ModeFlags};

    fn snap(content: &str) -> EngineSnapshot {
        let lines: Vec<&str> = content.split('\n').collect();
        let cols: u16 = lines
            .iter()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(1)
            .max(1) as u16;
        let rows: u16 = lines.len() as u16;
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
        for (row, line) in lines.iter().enumerate() {
            for (col, ch) in line.chars().enumerate() {
                cells[row * usize::from(cols) + col] = Cell {
                    ch: ch.to_string(),
                    width: 1,
                    fg: 0,
                    bg: 0,
                    attrs: 0,
                };
            }
        }
        EngineSnapshot {
            grid: CellGrid {
                cols,
                rows,
                cells,
                cursor: (0, 0),
            },
            modes: ModeFlags::default(),
            sequence: 1,
        }
    }

    fn info_for(comm: &str) -> PaneInfo {
        PaneInfo {
            argv: vec![format!("/usr/bin/{comm}")],
            comm: comm.to_string(),
            first_bytes: Vec::new(),
            env: std::collections::BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn generic_outline_sections_header_body_footer() {
        let outline = GenericAdapter
            .outline(&snap("title\nrow A\nrow B\nstatus"))
            .await
            .unwrap();
        assert_eq!(outline.nodes.len(), 3);
        assert_eq!(outline.nodes[0].role, "header");
        assert_eq!(outline.nodes[0].name, "title");
        assert_eq!(outline.nodes[1].role, "buffer");
        assert_eq!(outline.nodes[1].name, "row A\nrow B");
        assert_eq!(outline.nodes[2].role, "footer");
        assert_eq!(outline.nodes[2].name, "status");
    }

    #[tokio::test]
    async fn claude_code_detects_known_binaries() {
        for name in ["claude", "claude-code", "codex", "aider", "opencode"] {
            let score = ClaudeCodeAdapter.detect(&info_for(name)).await;
            assert!(score >= 0.9, "{name}: {score}");
        }
    }

    #[tokio::test]
    async fn claude_code_skips_unrelated() {
        let score = ClaudeCodeAdapter.detect(&info_for("bash")).await;
        assert!(score < f32::EPSILON, "got {score}");
    }

    #[tokio::test]
    async fn shell_detects_posix_shells() {
        for name in ["bash", "zsh", "fish", "sh", "dash", "ksh"] {
            let score = ShellAdapter.detect(&info_for(name)).await;
            assert!(score >= 0.85, "{name}: {score}");
        }
        let unrelated = ShellAdapter.detect(&info_for("vim")).await;
        assert!(unrelated < f32::EPSILON, "got {unrelated}");
    }

    #[tokio::test]
    async fn shell_outline_carries_prompt_node() {
        let outline = ShellAdapter
            .outline(&snap("user@host:~$ ls -la\nfile1 file2\nuser@host:~$ "))
            .await
            .unwrap();
        // Buffer + prompt nodes.
        assert_eq!(outline.adapter, "shell");
        assert!(outline.nodes.iter().any(|n| n.role == "prompt"));
    }

    #[tokio::test]
    async fn generic_empty_grid_returns_empty_nodes() {
        let outline = GenericAdapter.outline(&snap("\n\n")).await.unwrap();
        assert!(outline.nodes.is_empty());
    }

    // ----------------------------------------------------------------
    // Vim adapter tests
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn vim_detects_vim_family_binaries() {
        for name in ["vim", "nvim", "view", "ex", "vimtutor"] {
            let score = VimAdapter.detect(&info_for(name)).await;
            assert!(score >= 0.9, "{name}: expected >=0.9, got {score}");
        }
    }

    #[tokio::test]
    async fn vim_detects_through_bash_wrapper() {
        let mut info = info_for("bash");
        info.argv = vec!["bash".into(), "-c".into(), "/usr/bin/vimtutor".into()];
        let score = VimAdapter.detect(&info).await;
        assert!(
            score >= 0.6,
            "bash -c vimtutor: expected >=0.6, got {score}"
        );
    }

    #[tokio::test]
    async fn vim_skips_non_vim() {
        for name in ["bash", "claude", "lazygit", "tmux"] {
            let score = VimAdapter.detect(&info_for(name)).await;
            assert!(score < f32::EPSILON, "{name}: expected 0, got {score}");
        }
    }

    /// In normal mode the last row is empty; statusline shows file + [N/M].
    #[tokio::test]
    async fn vim_normal_mode_outline() {
        let outline = VimAdapter
            .outline(&snap("hello world\nsecond line\nsample.txt [1/2]\n"))
            .await
            .unwrap();
        assert_eq!(outline.adapter, "vim");
        let mode = outline.nodes.iter().find(|n| n.role == "mode").unwrap();
        assert_eq!(mode.name, "normal");
        let file = outline.nodes.iter().find(|n| n.role == "file").unwrap();
        assert_eq!(file.name, "sample.txt");
        assert!(file.value.is_none(), "not modified");
    }

    /// Insert mode: command-line row shows `-- INSERT --`.
    #[tokio::test]
    async fn vim_insert_mode_outline() {
        let outline = VimAdapter
            .outline(&snap("typing here\n\nsample.txt [1/1][+]\n-- INSERT --"))
            .await
            .unwrap();
        let mode = outline.nodes.iter().find(|n| n.role == "mode").unwrap();
        assert_eq!(mode.name, "insert");
        // [+] in statusline -> modified=true via `value: "modified"`.
        let file = outline.nodes.iter().find(|n| n.role == "file").unwrap();
        assert_eq!(file.name, "sample.txt");
        assert_eq!(file.value.as_ref().unwrap(), "modified");
    }

    /// Command mode: command-line row starts with `:`.
    #[tokio::test]
    async fn vim_command_mode_outline() {
        let outline = VimAdapter
            .outline(&snap("buffer\n\nsample.txt [1/1]\n:write"))
            .await
            .unwrap();
        let mode = outline.nodes.iter().find(|n| n.role == "mode").unwrap();
        assert_eq!(mode.name, "command");
        // command_line lives in the `value` field of the mode node.
        assert_eq!(mode.value.as_ref().unwrap(), ":write");
    }

    /// Search mode: command-line row starts with `/`.
    #[tokio::test]
    async fn vim_search_mode_outline() {
        let outline = VimAdapter
            .outline(&snap("buffer\n\nsample.txt [1/1]\n/foo"))
            .await
            .unwrap();
        let mode = outline.nodes.iter().find(|n| n.role == "mode").unwrap();
        assert_eq!(mode.name, "search");
        assert_eq!(mode.value.as_ref().unwrap(), "/foo");
    }

    /// Visual modes: command-line row shows `-- VISUAL --` (or its line/block
    /// variants).
    #[tokio::test]
    async fn vim_visual_modes_outline() {
        let cases = [
            ("-- VISUAL --", "visual"),
            ("-- VISUAL LINE --", "visual-line"),
            ("-- VISUAL BLOCK --", "visual-block"),
            ("-- REPLACE --", "replace"),
        ];
        for (marker, expected) in cases {
            let body = format!("buffer\n\nsample.txt [1/1]\n{marker}");
            let outline = VimAdapter.outline(&snap(&body)).await.unwrap();
            let mode = outline.nodes.iter().find(|n| n.role == "mode").unwrap();
            assert_eq!(mode.name, expected, "marker {marker:?}");
        }
    }

    /// Buffer body excludes statusline + command-line rows.
    #[tokio::test]
    async fn vim_buffer_excludes_chrome_rows() {
        let outline = VimAdapter
            .outline(&snap("first\nsecond\nsample.txt [2/2]\n-- INSERT --"))
            .await
            .unwrap();
        let buffer = outline.nodes.iter().find(|n| n.role == "buffer").unwrap();
        assert_eq!(buffer.name, "first\nsecond");
        assert!(!buffer.name.contains("sample.txt"));
        assert!(!buffer.name.contains("INSERT"));
    }

    /// When the statusline is missing (e.g. `set laststatus=0`), the
    /// adapter still emits a sensible outline with mode=normal and no
    /// file node.
    #[tokio::test]
    async fn vim_outline_without_statusline() {
        let outline = VimAdapter
            .outline(&snap("just\nbuffer\ncontent"))
            .await
            .unwrap();
        let mode = outline.nodes.iter().find(|n| n.role == "mode").unwrap();
        assert_eq!(mode.name, "normal");
        assert!(outline.nodes.iter().all(|n| n.role != "file"));
    }
}
