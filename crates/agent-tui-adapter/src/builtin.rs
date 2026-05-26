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
}
