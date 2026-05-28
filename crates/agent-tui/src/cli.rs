//! Clap-derived CLI surface.
//!
//! Mirrors `docs/RFC.md` §5. The grammar is locked; commands wire to the
//! daemon as their handlers land per phase (see `tracker.md`).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// agent-tui — a headless terminal browser for LLM agents.
#[derive(Debug, Parser)]
#[command(version, author, about, propagate_version = true)]
pub struct Cli {
    /// Global flags shared across every subcommand.
    #[command(flatten)]
    pub globals: GlobalArgs,
    /// The selected subcommand.
    #[command(subcommand)]
    pub command: Command,
}

/// Flags shared across every subcommand.
#[derive(Debug, Args, Clone)]
pub struct GlobalArgs {
    /// Session name; isolates daemons. Env: `AGENT_TUI_SESSION`.
    #[arg(
        long,
        env = "AGENT_TUI_SESSION",
        default_value = "default",
        global = true
    )]
    pub session: String,
    /// Override socket discovery root. Env: `AGENT_TUI_SOCKET_DIR`.
    #[arg(long, env = "AGENT_TUI_SOCKET_DIR", global = true)]
    pub socket_dir: Option<PathBuf>,
    /// Engine selection. v0.1.0 ships `alacritty` (default; only working engine);
    /// `wezterm` is a placeholder until a published substrate appears.
    #[arg(long, value_enum, default_value_t = EngineKind::Alacritty, global = true)]
    pub engine: EngineKind,
    /// JSON output for machine consumers.
    #[arg(long, global = true)]
    pub json: bool,
    /// Per-command timeout in milliseconds.
    #[arg(long, value_name = "MS", global = true)]
    pub timeout: Option<u64>,
    /// Wrap LLM-facing payloads in per-snapshot nonced boundary markers.
    #[arg(long, global = true)]
    pub content_boundaries: bool,
    /// Truncate snapshot payloads at N characters.
    #[arg(long, value_name = "N", global = true)]
    pub max_output: Option<usize>,
    /// Comma-separated allowlist of binary basenames `spawn` will accept.
    /// `*` allows everything (audit-only). Empty / unset = no restriction.
    /// Env: `AGENT_TUI_ALLOWED_BINARIES`.
    #[arg(
        long,
        env = "AGENT_TUI_ALLOWED_BINARIES",
        value_name = "CSV",
        global = true
    )]
    pub allowed_binaries: Option<String>,
}

/// VT engine selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum EngineKind {
    /// `alacritty_terminal`-backed engine. The v1 default — published on
    /// crates.io, MSRV matches ours. See `tracker.md` for substrate context.
    Alacritty,
    /// `wezterm-term`-backed engine. Placeholder; not yet on crates.io.
    Wezterm,
}

impl EngineKind {
    /// Wire-name written into the `<session>.engine` sidecar.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wezterm => "wezterm",
            Self::Alacritty => "alacritty",
        }
    }
}

/// CLI-side stdin mode (mirrors `agent_tui_protocol::request::StdinMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum StdinMode {
    /// Slave PTY (default). Programs see `isatty(0) == true`.
    #[default]
    Pty,
    /// Pipe. Programs see `isatty(0) == false`. Daemon retains the
    /// write end for later `stdin <bytes>` / `close-stdin` calls.
    Pipe,
    /// `/dev/null`. Programs that try to read see EOF immediately.
    Closed,
}

impl From<StdinMode> for agent_tui_protocol::request::StdinMode {
    fn from(m: StdinMode) -> Self {
        match m {
            StdinMode::Pty => Self::Pty,
            StdinMode::Pipe => Self::Pipe,
            StdinMode::Closed => Self::Closed,
        }
    }
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Spawn a PTY-backed pane running the given argv.
    Spawn {
        /// How to wire up the child's stdin. `pty` (default) gives a
        /// slave PTY for stdin — interactive programs need this.
        /// `pipe` gives a pipe — headless CLIs that do isatty(0) want
        /// this. `closed` ties stdin to /dev/null.
        #[arg(long, value_enum, default_value_t = StdinMode::Pty)]
        stdin: StdinMode,
        /// Working directory for the child. Inherits from the daemon
        /// when unset.
        #[arg(long, value_name = "PATH")]
        cwd: Option<String>,
        /// Set an environment variable for the child. Format `K=V`.
        /// Repeatable; later entries override earlier ones.
        #[arg(long = "env", value_name = "K=V")]
        env: Vec<String>,
        /// Argv to execute.
        #[arg(trailing_var_arg = true, num_args = 1..)]
        argv: Vec<String>,
    },
    /// List sessions / panes.
    List {
        /// Include panes across all sessions visible from this daemon.
        #[arg(long)]
        all: bool,
    },
    /// Snapshot the focused pane (or a specific one with `--pane`).
    Snapshot {
        /// Pane id (e.g. `p1`).
        #[arg(long)]
        pane: Option<String>,
        /// Snapshot mode.
        #[arg(long, value_enum, default_value_t = SnapshotMode::Outline)]
        mode: SnapshotMode,
        /// Also rasterize to this PNG path.
        #[arg(long, value_name = "PATH")]
        png: Option<PathBuf>,
        /// With `--png`, overlay numbered ref labels.
        #[arg(long)]
        annotate: bool,
        /// Filter the outline by a CSS-subset selector (see
        /// `docs/addressing-rfc.md` §2.2). Example:
        /// `--select '[role=buffer][focused]'`.
        #[arg(long, value_name = "SELECTOR")]
        select: Option<String>,
        /// With `--select`, return every matching node in depth-first
        /// pre-order. Without `--select` this is ignored.
        #[arg(long)]
        all: bool,
    },
    /// Press a key-token sequence (`"i hello<esc>:w<cr>"`).
    Press {
        /// Pane id; defaults to focused.
        #[arg(long)]
        pane: Option<String>,
        /// Key-token string. See `skills/core/references/keymap.md`.
        keys: String,
    },
    /// Type literal text at the focused pane.
    Type {
        /// Pane id; defaults to focused.
        #[arg(long)]
        pane: Option<String>,
        /// Literal UTF-8 text.
        text: String,
    },
    /// Send raw ANSI bytes (hex-encoded).
    SendAnsi {
        /// Pane id.
        #[arg(long)]
        pane: Option<String>,
        /// Hex-encoded byte string.
        bytes_hex: String,
    },
    /// Write bytes to the child's stdin pipe (only for `--stdin pipe` panes).
    /// Bytes can be passed literally as `--text` or hex-encoded as `--bytes-hex`.
    Stdin {
        #[arg(long)]
        pane: Option<String>,
        /// Literal UTF-8 text to push to stdin. Mutually exclusive with `--bytes-hex`.
        #[arg(long, conflicts_with = "bytes_hex")]
        text: Option<String>,
        /// Hex-encoded bytes to push to stdin. Mutually exclusive with `--text`.
        #[arg(long)]
        bytes_hex: Option<String>,
    },
    /// Close the child's stdin pipe (EOF). No-op for non-pipe panes.
    CloseStdin {
        #[arg(long)]
        pane: Option<String>,
    },
    /// Dump the CLI surface as JSON (hidden; used by xtask
    /// cli-coverage). Output shape:
    /// `{"subcommands":[{"name":"spawn","flags":["--stdin","..."],...}]}`.
    #[command(name = "__surface", hide = true)]
    DumpSurface,
    /// Ask an AI CLI a question, get the answer. Sugar over `run`
    /// that applies known recipes per CLI (claude → `claude -p`,
    /// opencode → `cat | opencode run --pure --title fixed
    /// --dangerously-skip-permissions`, …).
    ///
    /// `agent-tui ask claude "what is 40+2"` ≡ `agent-tui run --stdin
    /// "what is 40+2" -- claude -p`.
    Ask {
        /// AI CLI name (claude, opencode, pi, codex). Recipes for
        /// each are bundled in the binary.
        provider: String,
        /// The prompt text. Multiple positional args are joined with
        /// spaces.
        #[arg(trailing_var_arg = true)]
        prompt: Vec<String>,
        /// Per-invocation timeout in milliseconds.
        #[arg(long, default_value_t = 120_000)]
        max: u64,
    },
    /// Edit a file in `$EDITOR` (default vim) and return the
    /// resulting content once the editor exits.
    Edit {
        /// File path. The editor is spawned under agent-tui's PTY.
        path: String,
        /// Override `$EDITOR`. Default: env `$EDITOR` or `vim`.
        #[arg(long)]
        editor: Option<String>,
    },
    /// Watch a long-running command's output. Streams chunks to
    /// stdout until the child exits. Sugar over `spawn` + `tail
    /// --follow`.
    Watch {
        /// argv to exec.
        #[arg(trailing_var_arg = true, num_args = 1..)]
        argv: Vec<String>,
    },
    /// Replay an asciicast through a fresh engine and snapshot the
    /// result. Used for regression coverage: rerun saved sessions
    /// against the current engine and check that the rendered
    /// outline / text matches an expected snapshot.
    Replay {
        /// Path to the `.cast` file.
        cast: String,
        /// Optional path to an expected-snapshot JSON file (saved
        /// from a prior `--json snapshot --mode <mode>`). If set, the
        /// command exits non-zero on mismatch with a diff to stderr.
        #[arg(long, value_name = "PATH")]
        expect_snapshot: Option<String>,
        /// Snapshot mode for output. Default `outline`.
        #[arg(long, value_enum, default_value_t = SnapshotMode::Outline)]
        mode: SnapshotMode,
        /// Override geometry. Default 80×24.
        #[arg(long, default_value_t = 80)]
        cols: u16,
        /// Override row count. Default 24.
        #[arg(long, default_value_t = 24)]
        rows: u16,
    },
    /// Sugar verb: spawn a child, optionally feed it stdin, wait for
    /// it to exit, collect its output bytes, return everything in one
    /// JSON envelope. The "subprocess as data" pattern. Equivalent
    /// to `spawn --stdin pipe + stdin + close-stdin + wait --exit +
    /// tail --strip-ansi + die`, bundled.
    ///
    /// Most agent-driven invocations of headless CLIs (claude -p,
    /// gh api, gpg, ...) want this shape, not the interactive primitives.
    Run {
        /// Literal UTF-8 text to feed into the child's stdin. Mutually
        /// exclusive with `--stdin-file`. If neither is given, stdin
        /// is closed immediately (`/dev/null` from the child's POV).
        #[arg(long, value_name = "TEXT", conflicts_with = "stdin_file")]
        stdin: Option<String>,
        /// File whose contents are sent to the child's stdin.
        #[arg(long, value_name = "PATH")]
        stdin_file: Option<String>,
        /// Per-invocation timeout in milliseconds. Default 60s.
        #[arg(long, value_name = "MS", default_value_t = 60_000)]
        max: u64,
        /// Don't strip ANSI escape sequences from the returned bytes.
        /// Default behavior is `--strip-ansi`.
        #[arg(long)]
        raw: bool,
        /// Keep the per-session daemon running after this `run`
        /// returns. Default is to shut it down — `run` is one-shot.
        #[arg(long)]
        keep_daemon: bool,
        /// Working directory for the child.
        #[arg(long, value_name = "PATH")]
        cwd: Option<String>,
        /// Set environment variables. Format `K=V`. Repeatable.
        #[arg(long = "env", value_name = "K=V")]
        env: Vec<String>,
        /// argv to exec.
        #[arg(trailing_var_arg = true, num_args = 1..)]
        argv: Vec<String>,
    },
    /// Read raw bytes the child has written since `--since`. For
    /// headless CLIs where the agent wants the output stream, not the
    /// rendered terminal grid.
    Tail {
        /// Pane id; defaults to focused.
        #[arg(long)]
        pane: Option<String>,
        /// Cumulative byte offset to read from. Defaults to 0
        /// (everything still in the ring buffer).
        #[arg(long, default_value_t = 0)]
        since: u64,
        /// Strip ANSI escape sequences; return plain text in the
        /// `text` field instead of base64 in `bytes_b64`.
        #[arg(long)]
        strip_ansi: bool,
        /// Stream new bytes as they arrive instead of returning a
        /// single snapshot. Prints each chunk to stdout (or one
        /// NDJSON envelope per chunk under `--json`), terminating
        /// when the child exits.
        #[arg(long)]
        follow: bool,
    },
    /// Resize the focused pane.
    Resize {
        /// Pane id.
        #[arg(long)]
        pane: Option<String>,
        /// New column count.
        cols: u16,
        /// New row count.
        rows: u16,
    },
    /// Send a signal to a pane's child process group.
    Signal {
        /// Pane id.
        #[arg(long)]
        pane: Option<String>,
        /// Signal name (`SIGINT`, `SIGTERM`, ...) or number.
        signal: String,
    },
    /// Close a pane.
    Die {
        /// Pane id; defaults to focused.
        #[arg(long)]
        pane: Option<String>,
    },
    /// Pane focus management (`pane focus <id>`).
    Pane(PaneArgs),
    /// Wait for a state-change condition.
    Wait(WaitArgs),
    /// `eval` against an adapter (governed).
    Eval {
        /// Pane id.
        #[arg(long)]
        pane: Option<String>,
        /// Adapter to use (defaults to currently-attached).
        #[arg(long)]
        adapter: Option<String>,
        /// Expression string (adapter-specific).
        expr: String,
    },
    /// Daemon management.
    Daemon(DaemonArgs),
    /// `doctor` — environment / sanity / version-drift diagnostics.
    Doctor(DoctorArgs),
    /// MCP server mode — proxy the CLI surface as MCP tools over stdio.
    /// Lands in P4.
    Mcp(McpArgs),
    /// `skills get/list` — print embedded skill docs.
    Skills(SkillsArgs),
}

/// `pane` subcommand group.
#[derive(Debug, Args)]
pub struct PaneArgs {
    /// What to do with the pane focus.
    #[command(subcommand)]
    pub action: PaneAction,
}

/// Pane subcommand actions.
#[derive(Debug, Subcommand)]
pub enum PaneAction {
    /// Set the focused pane.
    Focus {
        /// Pane id (e.g. `p1`). Pass `none` to clear focus.
        pane: String,
    },
}

/// `wait` subcommand. Exactly one mode flag is required.
///
/// The `wait_mode` group is declared on the mode arguments themselves so
/// `--pane` and `--max` stay outside the mutual-exclusion set.
#[derive(Debug, Args, Clone)]
pub struct WaitArgs {
    /// Pane id; defaults to focused.
    #[arg(long, global = false)]
    pub pane: Option<String>,
    /// Block until next mutation past `<seq>`. Primary wait primitive.
    #[arg(long, group = "wait_mode")]
    pub since: Option<u64>,
    /// Sugar over `--since` using the seq→hash window.
    #[arg(long, group = "wait_mode")]
    pub hash: Option<String>,
    /// No new mutations for N ms.
    #[arg(long, group = "wait_mode")]
    pub idle: Option<u64>,
    /// Visible buffer matches regex.
    #[arg(long, group = "wait_mode")]
    pub text: Option<String>,
    /// Cursor stable for N ms.
    #[arg(long, group = "wait_mode")]
    pub cursor_stable: Option<u64>,
    /// Alt-screen on/off toggle.
    #[arg(long, group = "wait_mode")]
    pub alt_screen: Option<bool>,
    /// Pane's child exits.
    #[arg(long, group = "wait_mode")]
    pub exit: bool,
    /// Block until a selector matches a node in the outline. With
    /// `--gone`, block until no node matches.
    /// Example: `--ref '[role=cmdline][focused]'`.
    /// See `docs/addressing-rfc.md` §2.2 for the selector grammar.
    #[arg(long = "ref", group = "wait_mode", value_name = "SELECTOR")]
    pub ref_selector: Option<String>,
    /// Inverts `--ref`: wait until the selector matches NOTHING.
    /// Useful for "wait for the confirm to dismiss".
    #[arg(long, requires = "ref_selector")]
    pub gone: bool,
    /// Mandatory total timeout in ms (default 25000).
    #[arg(long, default_value_t = 25_000)]
    pub max: u64,
}

/// `daemon` subcommand group.
#[derive(Debug, Args)]
pub struct DaemonArgs {
    /// What to do with the daemon.
    #[command(subcommand)]
    pub action: DaemonAction,
}

/// What the `daemon` subcommand can do.
#[derive(Debug, Subcommand)]
pub enum DaemonAction {
    /// Run the daemon in the foreground. Used by `agent-tui` itself when
    /// the CLI spawns the daemon. Humans usually don't run this directly.
    Run {
        /// PID of the process whose death should also shut down the
        /// daemon. The CLI's lazy-spawn path passes its own PID here
        /// so a `cargo test` panic or a SIGKILL'd test runner takes
        /// the daemon down with it instead of orphaning a daemon to
        /// PID 1.
        #[arg(long, value_name = "PID")]
        monitor_parent: Option<u32>,

        /// Shut down after this many seconds of no client activity.
        /// Defaults to 900s (15 min); overridable via
        /// `AGENT_TUI_IDLE_TIMEOUT` env. Set to `0` to disable.
        #[arg(long, value_name = "SECS", env = "AGENT_TUI_IDLE_TIMEOUT")]
        idle_timeout_secs: Option<u64>,
    },
    /// Print daemon status (running / unreachable / version).
    Status,
    /// Initiate idle-shutdown.
    Shutdown {
        /// Accept loss of non-shell pane state.
        #[arg(long)]
        force: bool,
    },
}

/// `doctor` subcommand args.
#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Quick / cheap subset of checks.
    #[arg(long)]
    pub quick: bool,
    /// Apply destructive repairs (kill stale daemons, clear stale sidecars).
    #[arg(long)]
    pub fix: bool,
    /// Write a tarball diagnostic bundle to this path.
    #[arg(long, value_name = "PATH")]
    pub diagnostic_bundle: Option<PathBuf>,
}

/// `mcp` subcommand args.
#[derive(Debug, Args)]
pub struct McpArgs {
    /// Run the MCP server. Reads MCP JSON-RPC on stdin, writes on stdout.
    #[command(subcommand)]
    pub action: McpAction,
}

/// MCP subcommand actions.
#[derive(Debug, Subcommand)]
pub enum McpAction {
    /// Serve the MCP protocol over stdio.
    Serve,
}

/// `skills` subcommand args.
#[derive(Debug, Args)]
pub struct SkillsArgs {
    /// What to do.
    #[command(subcommand)]
    pub action: SkillsAction,
}

/// Skills subcommand actions.
#[derive(Debug, Subcommand)]
pub enum SkillsAction {
    /// List embedded skills.
    List,
    /// Print a skill to stdout.
    Get {
        /// Skill name (e.g. `core`).
        name: String,
        /// Include references and templates.
        #[arg(long)]
        full: bool,
    },
}

/// Snapshot rendering mode (CLI side).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SnapshotMode {
    /// Outline only. Default.
    Outline,
    /// RLE-compressed cell grid only.
    Cells,
    /// Adapter tree only.
    Adapter,
    /// Visible cells as a plain UTF-8 string (rows joined with `\n`).
    Text,
    /// Outline + cells + adapter + text.
    Hybrid,
}

impl From<SnapshotMode> for agent_tui_protocol::request::SnapshotMode {
    fn from(m: SnapshotMode) -> Self {
        match m {
            SnapshotMode::Outline => Self::Outline,
            SnapshotMode::Cells => Self::Cells,
            SnapshotMode::Adapter => Self::Adapter,
            SnapshotMode::Text => Self::Text,
            SnapshotMode::Hybrid => Self::Hybrid,
        }
    }
}
