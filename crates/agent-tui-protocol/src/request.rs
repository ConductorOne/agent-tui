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
    /// Block until a selector matches a node in the outline (or, with
    /// `gone: true`, until no node matches). See
    /// `docs/addressing-rfc.md` §2.2.
    Ref {
        /// Selector expression (CSS-subset; parsed by the daemon).
        selector: String,
        /// If true, fire when the selector matches NOTHING (useful for
        /// "wait for the confirm to dismiss").
        #[serde(default)]
        gone: bool,
    },
}

/// Where the child's stdin file descriptor comes from. Spawn-time choice.
///
/// Default is `Pty` (the slave PTY's fd) for backwards compatibility and
/// because interactive apps need a TTY for stdin (canonical mode, line
/// editing, signals). `Pipe` is the right answer for headless CLIs that
/// do `isatty(0)` checks and refuse a TTY stdin (e.g. `claude -p`,
/// `gh api`). `Closed` ties stdin to `/dev/null` for programs that don't
/// read stdin but might inherit it accidentally.
///
/// Stdout and stderr are always the slave PTY — the engine needs to
/// observe rendered output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StdinMode {
    /// Slave PTY (default). Programs see `isatty(0) == true`.
    #[default]
    Pty,
    /// Pipe. Programs see `isatty(0) == false`. Daemon retains the
    /// write end so callers can `stdin <bytes>` and `close-stdin`.
    Pipe,
    /// `/dev/null`. Programs that try to read will see EOF immediately.
    Closed,
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
    /// Visible cells flattened to a plain UTF-8 string. Rows joined
    /// with `\n`, trailing whitespace stripped. The right answer when
    /// the pane is unstructured text and you want "what does the
    /// screen say."
    Text,
    /// Outline + cells + adapter + text.
    Hybrid,
}

/// `attach` prelude shape: how the consumer's initial screen is seeded before
/// the live byte-follow takes over.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreludeKind {
    /// A rendered current-screen frame (text/cells) captured atomically with
    /// the follow offset. Default — the right answer for an alt-screen TUI.
    #[default]
    Rendered,
    /// Raw ring bytes from `since` (generalizes `tail --since`).
    Raw,
    /// No prelude; skip straight to follow from the current high-water mark.
    None,
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
        /// How to wire up the child's stdin. Default `Pty` matches
        /// pre-RFC behavior. `Pipe` gives the child a pipe (not a
        /// TTY) for stdin, which programs like `claude -p` need to
        /// not refuse to read. `Closed` ties stdin to `/dev/null`.
        #[serde(default)]
        stdin: StdinMode,
        /// Environment-variable overrides for the child. These ADD
        /// to or override the daemon's inherited environment; they
        /// don't clear it.
        #[serde(default)]
        env: Vec<(String, String)>,
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
        /// With `png`, overlay ref bounding boxes + labels. `Some(sel)`
        /// restricts the overlay to refs matching selector `sel`; `Some("")`
        /// annotates every ref; `None` disables annotation. Meaningless
        /// without `png` (the handler rejects it).
        #[serde(default)]
        annotate: Option<String>,
        /// Optional selector to filter the outline (RFC §2.2). When
        /// present, the response's `outline.nodes` is restricted to
        /// matching nodes; non-matching parents are pruned. Forces
        /// `mode` to include an outline.
        #[serde(default)]
        select: Option<String>,
        /// With `select`, return every match in depth-first pre-order.
        /// Without `select`, ignored. Default false → first match only.
        #[serde(default)]
        all: bool,
        /// With `mode = text` (or `hybrid`), reconstruct per-cell SGR
        /// escape sequences instead of emitting plain UTF-8. Lets a
        /// caller preserve color when presenting to a human or
        /// debugging. Ignored by non-text modes.
        #[serde(default)]
        keep_color: bool,
    },
    /// Press the given key-token sequence at the focused pane.
    Press {
        /// Pane id; `None` = focused.
        #[serde(default)]
        pane: Option<PaneId>,
        /// Key tokens as a single string (e.g. `"i hello<esc>:w<cr>"`).
        keys: String,
        /// Optional selector identifying the **target** node within the
        /// pane. The pane's attached adapter translates `(target, keys)`
        /// into PTY bytes via `Adapter::route` (RFC §2.3). When unset,
        /// the keys go straight to the PTY (identity routing).
        #[serde(default)]
        to: Option<String>,
        /// Write-lease token (from an `attach --write-lease` prelude). When a
        /// pane has an outstanding lease, only the holder's token may write.
        #[serde(default)]
        lease: Option<Uuid>,
    },
    /// Type literal text at the focused pane (no key interpretation).
    Type {
        /// Pane id; `None` = focused.
        #[serde(default)]
        pane: Option<PaneId>,
        /// Literal UTF-8 text.
        text: String,
        /// Optional selector identifying the target node. See `Press.to`.
        #[serde(default)]
        to: Option<String>,
        /// Write-lease token; see [`Command::Press`].
        #[serde(default)]
        lease: Option<Uuid>,
    },
    /// Send raw bytes (ANSI escape sequences, mouse events, OSC, DCS).
    SendAnsi {
        /// Pane id; `None` = focused.
        #[serde(default)]
        pane: Option<PaneId>,
        /// Hex-encoded raw bytes.
        bytes_hex: String,
        /// Write-lease token; see [`Command::Press`].
        #[serde(default)]
        lease: Option<Uuid>,
    },
    /// Write bytes to the child's stdin **pipe**. Only works for panes
    /// spawned with `stdin: Pipe`; returns an error otherwise. For
    /// PTY-stdin panes, use `type` / `press` / `send-ansi` instead —
    /// those write through the master PTY (which IS stdin for PTY mode).
    Stdin {
        /// Pane id; `None` = focused.
        #[serde(default)]
        pane: Option<PaneId>,
        /// Hex-encoded raw bytes to push to the pipe.
        bytes_hex: String,
        /// Write-lease token; see [`Command::Press`].
        #[serde(default)]
        lease: Option<Uuid>,
    },
    /// Close the child's stdin pipe (EOF). Only meaningful for panes
    /// spawned with `stdin: Pipe`; no-op otherwise.
    CloseStdin {
        /// Pane id; `None` = focused.
        #[serde(default)]
        pane: Option<PaneId>,
    },
    /// Return the raw bytes the child has written to stdout+stderr
    /// since `since` (a cumulative byte offset). Useful for headless
    /// CLIs (`subprocess as data` pattern) where the agent wants the
    /// program's output stream, not the rendered terminal grid.
    ///
    /// **Streaming mode.** When `follow: true`, the daemon emits
    /// multiple `ResponseEnvelope` lines on the same connection,
    /// each carrying a chunk of newly-arrived bytes, terminated by
    /// a final envelope with `data.type == "eof"`. Clients NOT
    /// expecting streaming should call with `follow: false` (default).
    Tail {
        /// Pane id; `None` = focused.
        #[serde(default)]
        pane: Option<PaneId>,
        /// Cumulative byte offset to read from. `0` returns
        /// everything still in the ring buffer.
        #[serde(default)]
        since: u64,
        /// If true, strip ANSI/CSI escape sequences from the
        /// returned bytes. Useful when the agent wants plain text.
        #[serde(default)]
        strip_ansi: bool,
        /// If true, stream new bytes as they arrive instead of
        /// returning a single snapshot. The daemon emits one
        /// envelope per chunk plus a final `{type:"eof"}` envelope.
        #[serde(default)]
        follow: bool,
    },
    /// Attach to a pane as a live viewer: a streaming connection that emits an
    /// atomic **prelude** (rendered current frame + the follow offset captured
    /// under one lock) followed by raw byte **chunks** from that exact offset,
    /// then a terminal **eof**. Closes the `snapshot`/`tail` coordinate-system
    /// gap (no TOCTOU) so many late joiners can fan out from one PTY.
    Attach {
        /// Pane id; `None` = focused.
        #[serde(default)]
        pane: Option<PaneId>,
        /// Prelude shape (rendered / raw / none).
        #[serde(default)]
        prelude: PreludeKind,
        /// Rendered-prelude format (`text` or `cells`). Ignored for raw/none.
        #[serde(default)]
        mode: SnapshotMode,
        /// Raw-prelude start offset (with `prelude == raw`).
        #[serde(default)]
        since: u64,
        /// Request the single-writer lease for this pane (opt-in).
        #[serde(default)]
        write_lease: bool,
        /// Strip ANSI/CSI escapes from follow chunks (and a raw prelude).
        #[serde(default)]
        strip_ansi: bool,
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
    /// Close a pane. Group-aware teardown so the harness's forked
    /// subprocesses don't get orphaned. With `grace`, SIGTERM the process
    /// group, poll for exit up to `grace`, then SIGKILL the group if still
    /// alive. Without `grace`, SIGTERM the group immediately (no wait).
    Die {
        /// Pane id; `None` = focused.
        #[serde(default)]
        pane: Option<PaneId>,
        /// Graceful teardown window. `None` = immediate group SIGTERM.
        #[serde(default, with = "opt_duration_ms")]
        grace: Option<Duration>,
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
    /// Set the focused pane for this session. Subsequent no-`--pane`
    /// commands resolve to this pane. Pass `pane: None` to clear focus.
    Focus {
        /// Pane to focus. `None` clears the current focus.
        #[serde(default)]
        pane: Option<PaneId>,
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

/// Like [`duration_ms`] but for an optional window: serializes to a JSON
/// number of milliseconds when present, `null`/absent when `None`. Used by
/// `die`'s `grace` field.
mod opt_duration_ms {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    // serde's `with` adaptor dictates the `&Option<_>` receiver shape.
    #[allow(clippy::ref_option)]
    pub(super) fn serialize<S: Serializer>(
        d: &Option<Duration>,
        ser: S,
    ) -> Result<S::Ok, S::Error> {
        match d {
            Some(d) => {
                let ms = u64::try_from(d.as_millis()).map_err(serde::ser::Error::custom)?;
                ser.serialize_some(&ms)
            }
            None => ser.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        de: D,
    ) -> Result<Option<Duration>, D::Error> {
        let opt = Option::<u64>::deserialize(de)?;
        Ok(opt.map(Duration::from_millis))
    }
}
