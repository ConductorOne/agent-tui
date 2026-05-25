//! Request envelope and the [`Command`] enum that drives the daemon.
//!
//! Every wire request is a single JSON line of the form
//! `{"id": "uuid", "command": { ... }}` where the embedded command picks
//! one variant of [`Command`].

use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

use crate::PaneId;

/// Wait-condition discriminator for `wait`.
///
/// See `docs/RFC.md` §4.5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitCondition {
    /// Block until the pane's sequence exceeds `since`.
    Since {
        /// The pane sequence to wait past.
        since: u64,
    },
    /// Sugar over `Since` via the daemon's seq→hash window.
    Hash {
        /// Hex-encoded SHA-256 of a prior snapshot.
        hash: String,
    },
    /// Block until there have been no mutations for `quiet_ms`.
    Idle {
        /// Milliseconds of no-mutation that count as idle.
        quiet_ms: u64,
    },
    /// Block until the visible buffer matches `regex`.
    Text {
        /// A Rust-flavor regular expression.
        regex: String,
    },
    /// Block until any cell in the region `(r1,c1)-(r2,c2)` changes.
    Cells {
        /// Region anchor (top-left): row.
        r1: u16,
        /// Region anchor (top-left): col.
        c1: u16,
        /// Region anchor (bot-right): row.
        r2: u16,
        /// Region anchor (bot-right): col.
        c2: u16,
    },
    /// Block until the cursor position has been stable for `stable_ms`.
    CursorStable {
        /// Milliseconds the cursor must be stable.
        stable_ms: u64,
    },
    /// Block until the pane's alt-screen flag matches `on`.
    AltScreen {
        /// Target value.
        on: bool,
    },
    /// Block until the pane's PTY child exits.
    Exit,
}

/// Snapshot rendering mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotMode {
    /// Outline only. Default.
    #[default]
    Outline,
    /// RLE-compressed cell grid only.
    Cells,
    /// Adapter tree only (no outline traversal).
    Adapter,
    /// Outline + cells + adapter.
    Hybrid,
}

/// The set of commands the daemon accepts. Each enum variant maps 1:1 to a
/// CLI subcommand.
///
/// See `docs/RFC.md` §5.1.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    /// Spawn a new PTY-backed pane.
    Spawn {
        /// argv to exec.
        argv: Vec<String>,
        /// cwd; `None` inherits from daemon.
        #[serde(default)]
        cwd: Option<String>,
        /// Optional initial size as `(cols, rows)`.
        #[serde(default)]
        size: Option<(u16, u16)>,
    },
    /// List panes / sessions.
    List {
        /// If true, includes panes across all sessions visible from this daemon.
        #[serde(default)]
        all: bool,
    },
    /// Snapshot a pane.
    Snapshot {
        /// Pane id; `None` = currently-focused pane in this session.
        #[serde(default)]
        pane: Option<PaneId>,
        /// Snapshot rendering mode.
        #[serde(default)]
        mode: SnapshotMode,
        /// If true, also rasterize to PNG and return the on-disk path.
        #[serde(default)]
        png: Option<String>,
        /// If true (with `png`), overlay numbered ref labels.
        #[serde(default)]
        annotate: bool,
    },
    /// Press the given key-token sequence at the focused pane.
    Press {
        /// Pane id; `None` = focused.
        #[serde(default)]
        pane: Option<PaneId>,
        /// Key tokens as a single string (e.g. `"i hello<esc>:w<cr>"`).
        keys: String,
    },
    /// Type literal text at the focused pane (no key interpretation).
    Type {
        /// Pane id; `None` = focused.
        #[serde(default)]
        pane: Option<PaneId>,
        /// Literal UTF-8 text.
        text: String,
    },
    /// Send raw bytes (ANSI escape sequences, mouse events, OSC, DCS).
    SendAnsi {
        /// Pane id; `None` = focused.
        #[serde(default)]
        pane: Option<PaneId>,
        /// Hex-encoded raw bytes.
        bytes_hex: String,
    },
    /// Resize a pane.
    Resize {
        /// Pane id; `None` = focused.
        #[serde(default)]
        pane: Option<PaneId>,
        /// Number of columns.
        cols: u16,
        /// Number of rows.
        rows: u16,
    },
    /// Send a UNIX signal to a pane's foreground process group.
    Signal {
        /// Pane id; `None` = focused.
        #[serde(default)]
        pane: Option<PaneId>,
        /// Signal name (`SIGINT`, `SIGTERM`, ...) or number.
        signal: String,
    },
    /// Close a pane (graceful → SIGKILL after `--timeout`).
    Die {
        /// Pane id; `None` = focused.
        #[serde(default)]
        pane: Option<PaneId>,
    },
    /// Wait for a state-change condition.
    Wait {
        /// Pane id; `None` = focused.
        #[serde(default)]
        pane: Option<PaneId>,
        /// The condition to wait on.
        condition: WaitCondition,
        /// Mandatory total timeout.
        #[serde(with = "duration_ms")]
        timeout: Duration,
    },
    /// Adapter `eval` against the focused pane's adapter.
    Eval {
        /// Pane id; `None` = focused.
        #[serde(default)]
        pane: Option<PaneId>,
        /// Optional adapter override; default = currently-attached.
        #[serde(default)]
        adapter: Option<String>,
        /// The expression string. Adapter-specific.
        expr: String,
    },
    /// Daemon status query (used by `doctor` and the CLI's reachability check).
    DaemonStatus,
    /// Initiate idle-shutdown of the daemon (optionally forced).
    DaemonShutdown {
        /// If true, accept loss of non-shell pane state.
        #[serde(default)]
        force: bool,
    },
}

/// Top-level wire request envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Correlation id chosen by the client.
    pub id: Uuid,
    /// Wire-protocol version advertised by the client.
    pub protocol: u32,
    /// The actual command.
    pub command: Command,
}

/// Serde adapter that serializes `Duration` as integer milliseconds.
mod duration_ms {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub(super) fn serialize<S: Serializer>(d: &Duration, ser: S) -> Result<S::Ok, S::Error> {
        u64::try_from(d.as_millis())
            .map_err(serde::ser::Error::custom)?
            .serialize(ser)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Duration, D::Error> {
        let ms = u64::deserialize(de)?;
        Ok(Duration::from_millis(ms))
    }
}
