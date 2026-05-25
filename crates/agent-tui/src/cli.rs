//! Clap-derived CLI surface.
//!
//! Mirrors `docs/RFC.md` §5. v0.1.0 only wires the subcommands the daemon
//! actually handles end-to-end (`daemon`, `status`, `shutdown`, plus
//! placeholders that return `Internal` for the others). The grammar shape
//! is locked in so the surface area stops moving while the engine, adapter,
//! and recorder land in P0–P2.

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
    /// Engine selection. v0.1.0 ships `wezterm` only; `alacritty` lands in P5.
    #[arg(long, value_enum, default_value_t = EngineKind::Wezterm, global = true)]
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
}

/// VT engine selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum EngineKind {
    /// `wezterm-term`-backed engine. Default.
    Wezterm,
    /// `alacritty-terminal`-backed engine. Lean alternative; lands in P5.
    Alacritty,
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

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Spawn a PTY-backed pane. Stub in v0.1.0.
    Spawn {
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

/// `wait` subcommand. Exactly one mode flag is required.
#[derive(Debug, Args, Clone)]
#[group(required = true, multiple = false, id = "wait_mode")]
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
    Run,
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
    /// Outline + cells + adapter.
    Hybrid,
}

impl From<SnapshotMode> for agent_tui_protocol::request::SnapshotMode {
    fn from(m: SnapshotMode) -> Self {
        match m {
            SnapshotMode::Outline => Self::Outline,
            SnapshotMode::Cells => Self::Cells,
            SnapshotMode::Adapter => Self::Adapter,
            SnapshotMode::Hybrid => Self::Hybrid,
        }
    }
}
