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
    /// Engine selection. Only `alacritty` (the default) is implemented;
    /// `wezterm` is an (unimplemented) placeholder and selecting it is an
    /// error until a published substrate appears.
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
    /// (unimplemented) `wezterm-term`-backed engine. Placeholder; not yet
    /// on crates.io. Selecting it errors — only `alacritty` works today.
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
        /// With `--mode text` (or `hybrid`), keep color: reconstruct
        /// per-cell SGR escapes instead of stripping them. Ignored by
        /// other modes.
        #[arg(long)]
        keep_color: bool,
    },
    /// Press a key-token sequence (`"i hello<esc>:w<cr>"`).
    Press {
        /// Pane id; defaults to focused.
        #[arg(long)]
        pane: Option<String>,
        /// Selector identifying the target node within the pane. The
        /// adapter translates `(target, keys)` into PTY bytes (RFC
        /// §2.3). When omitted, the keys go straight to the PTY.
        /// Example: `--to '@tmux.pane[%2]'`.
        #[arg(long, value_name = "SELECTOR")]
        to: Option<String>,
        /// Key-token string. See `skills/core/references/keymap.md`.
        keys: String,
    },
    /// Type literal text at the focused pane.
    Type {
        /// Pane id; defaults to focused.
        #[arg(long)]
        pane: Option<String>,
        /// Selector identifying the target node. See `Press.to`.
        #[arg(long, value_name = "SELECTOR")]
        to: Option<String>,
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
        /// Graceful teardown: SIGTERM the child's process group, wait up to
        /// `<ms>` for it to exit, then SIGKILL the group if still alive.
        /// Without this flag, `die` sends a single SIGTERM to the group and
        /// returns immediately. Given without a value, defaults to 3000 ms.
        #[arg(long, value_name = "ms", num_args = 0..=1, default_missing_value = "3000")]
        grace: Option<u64>,
    },
    /// Pane focus management (`pane focus <id>`).
    Pane(PaneArgs),
    /// Wait for a state-change condition.
    Wait(WaitArgs),
    /// (unimplemented) Evaluate an adapter expression. Not wired in this
    /// build — returns an error naming the alternative. For reading
    /// structured screen state today, use `snapshot --select <selector>`.
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
    /// Session state management (`session gc`).
    Session(SessionArgs),
    /// `doctor` — environment / sanity / version-drift diagnostics.
    Doctor(DoctorArgs),
    /// MCP server mode — serve agent-tui's capabilities as MCP tools over
    /// stdio (JSON-RPC). `mcp serve` exposes `spawn`, `snapshot`
    /// (`outline`/`text`/`cells`/`adapter`/`hybrid`), `press`, `type`,
    /// `wait`, `die`, `focus`, `list`, and `daemon_status` to MCP clients.
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

/// `session` subcommand group.
#[derive(Debug, Args)]
pub struct SessionArgs {
    /// What to do with session state.
    #[command(subcommand)]
    pub action: SessionAction,
}

/// `session` subcommand actions.
#[derive(Debug, Subcommand)]
pub enum SessionAction {
    /// Garbage-collect dead sessions' on-disk state — sidecar files in
    /// the socket root plus cast dirs under `$XDG_STATE_HOME`. Never
    /// touches a session whose daemon is still answering its socket.
    Gc {
        /// Only prune dead sessions whose most-recent state is at least
        /// this many days old. Ignored with `--all`.
        #[arg(long, value_name = "DAYS", default_value_t = 7)]
        older_than_days: u64,
        /// Prune every dead session regardless of age.
        #[arg(long)]
        all: bool,
        /// Report what would be pruned without deleting anything.
        #[arg(long)]
        dry_run: bool,
    },
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
    /// Block until the event sequence advances past `<seq>`. Primary
    /// wait primitive. `--sequence` is a visible alias of this flag
    /// (same semantics: wait until the event stream passes `<seq>`).
    #[arg(long, visible_alias = "sequence", group = "wait_mode")]
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

#[cfg(test)]
mod tests {
    use super::{Cli, Command};
    use clap::{CommandFactory, Parser};
    use std::collections::BTreeSet;

    /// `--sequence` is a visible alias of `--since` (RFC P1-1): the
    /// canonical docs use `wait --sequence <n>`, which used to error
    /// (`unexpected argument`). Parse the real CLI and assert the alias
    /// lands on the same field with the same value.
    #[test]
    fn wait_sequence_is_visible_alias_of_since() {
        let cli = Cli::try_parse_from(["agent-tui", "wait", "--sequence", "7"])
            .expect("`wait --sequence 7` must parse");
        match cli.command {
            Command::Wait(w) => {
                assert_eq!(w.since, Some(7), "--sequence must set the `since` field");
            }
            other => panic!("expected Wait, got {other:?}"),
        }
        // And the original spelling still works identically.
        let cli2 = Cli::try_parse_from(["agent-tui", "wait", "--since", "7"]).unwrap();
        assert!(matches!(cli2.command, Command::Wait(w) if w.since == Some(7)));
    }

    /// Recursively collect every long flag + visible alias the real clap
    /// surface exposes (e.g. `--since`, `--sequence`).
    fn real_flags() -> BTreeSet<String> {
        fn walk(cmd: &clap::Command, out: &mut BTreeSet<String>) {
            for a in cmd.get_arguments() {
                if let Some(long) = a.get_long() {
                    out.insert(format!("--{long}"));
                }
                if let Some(aliases) = a.get_visible_aliases() {
                    out.extend(aliases.into_iter().map(|al| format!("--{al}")));
                }
            }
            for s in cmd.get_subcommands() {
                if s.get_name() != "help" {
                    walk(s, out);
                }
            }
        }
        let mut set = BTreeSet::new();
        walk(&Cli::command(), &mut set);
        // clap-injected flags that may not surface as positional args.
        set.insert("--help".into());
        set.insert("--version".into());
        set
    }

    /// Extract `--flag` tokens from inside ```` ``` ```` fenced code
    /// blocks only — the usage synopses. Prose mentions (e.g. "the
    /// `--foo` flag") are commentary, not normative, and are skipped.
    fn fenced_flag_tokens(md: &str) -> BTreeSet<String> {
        let mut toks = BTreeSet::new();
        let mut in_fence = false;
        for line in md.lines() {
            if line.trim_start().starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            if !in_fence {
                continue;
            }
            let bytes = line.as_bytes();
            let mut i = 0;
            while i + 2 < bytes.len() {
                if bytes[i] == b'-' && bytes[i + 1] == b'-' && bytes[i + 2].is_ascii_lowercase() {
                    let start = i + 2;
                    let mut j = start;
                    while j < bytes.len()
                        && (bytes[j].is_ascii_lowercase()
                            || bytes[j].is_ascii_digit()
                            || bytes[j] == b'-')
                    {
                        j += 1;
                    }
                    toks.insert(format!("--{}", &line[start..j]));
                    i = j;
                } else {
                    i += 1;
                }
            }
        }
        toks
    }

    /// Doc-vs-`--help` conformance (RFC P1-1): every `--flag` documented
    /// in a usage synopsis in `commands.md` must exist in the real clap
    /// surface. Guards against the `--sequence`-style drift where the doc
    /// named a flag the binary never had. Runs under `cargo test
    /// --workspace`, so CI fails on future drift.
    #[test]
    fn commands_md_flags_all_exist_in_cli() {
        const COMMANDS_MD: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/skill-data/core/references/commands.md"
        ));
        let real = real_flags();
        let documented = fenced_flag_tokens(COMMANDS_MD);
        let invented: Vec<&String> = documented.iter().filter(|t| !real.contains(*t)).collect();
        assert!(
            invented.is_empty(),
            "commands.md documents flag(s) that do not exist in the CLI: {invented:?}\n\
             (regenerate the synopsis from `agent-tui <sub> --help`; \
             real flags: {real:?})"
        );
    }
}
