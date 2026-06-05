# Commands

Every `agent-tui` subcommand, flag, and alias. This file is meant to
be regeneratable from `agent-tui --help` — when the CLI changes,
update this file in the same PR. This is **enforced**: the
`commands_md_flags_all_exist_in_cli` test (run by CI's `cargo test
--workspace`) and `cargo xtask help-conformance` both fail if this
file names a `--flag` that doesn't exist in clap, so drift can't
silently return.

## Global options
<!-- tested-by: navigation -->

```
--session <NAME>            Session name; isolates daemons (env: AGENT_TUI_SESSION)
--socket-dir <DIR>          Override socket discovery root (env: AGENT_TUI_SOCKET_DIR)
--engine <ENGINE>           alacritty (default) | wezterm
--json                      JSON output for machine consumers
--timeout <MS>              Per-command timeout (ms)
--content-boundaries        Wrap snapshot payloads in nonced boundary markers
--max-output <N>            Truncate snapshot payloads at N chars
--allowed-binaries <CSV>    Allowlist of binary basenames `spawn` accepts (`*` = any)
-h, --help                  Print help
-V, --version               Print version
```

## Subcommands
<!-- tested-by: navigation -->

### `run` (sugar)

```
agent-tui run [--stdin <text>] [--stdin-file <path>] [--max <ms>]
              [--raw] [--keep-daemon] [--cwd <path>] [--env K=V]... -- <argv...>
```

Sugar verb: spawn + optionally write stdin + close-stdin + wait for
exit + tail bytes + die + shut daemon. The "subprocess as data"
pattern in one verb. Default `--stdin <text>` writes literal bytes
then closes; `--raw` returns bytes-with-escapes instead of stripped
text; `--keep-daemon` skips the daemon shutdown step.

`--cwd <path>` sets the child's working directory; `--env K=V`
(repeatable) sets environment variables — together they replace the
`bash -c "cd …; K=V …"` wrapper most workflows used to need.

`--max <ms>` is the child's deadline. It is honored end-to-end: the
client read timeout is derived from it (plus a safety margin), so a
long `--max` no longer trips a fixed client-side cap.

### `spawn`

```
agent-tui spawn [--stdin pty|pipe|closed] [--cwd <path>] [--env K=V]...
                [--cols <N>] [--rows <N>] -- <argv...>
```

Spawn a PTY-backed pane running `argv`. Returns the pane id (e.g.
`p1`). Honors the `--allowed-binaries` allowlist.

`--stdin pty` (default) gives the slave PTY for stdin. `--stdin
pipe` gives a kernel pipe — required by CLIs that do `isatty(0)`
checks. `--stdin closed` ties stdin to `/dev/null`. `--cwd` /
`--env` set the child's working directory and environment.

### `list`

```
agent-tui list
```

List the panes in the current session. Each entry reports the pane id,
argv, spawn time, and its **current** geometry (`cols`/`rows`) — the live
size after any `resize`, not the spawn-time dimensions.

### `snapshot`

```
agent-tui snapshot [--pane <id>] [--mode outline|text|cells|adapter|hybrid]
                   [--png <path>] [--annotate] [--keep-color]
```

Snapshot the focused pane (or a specific one with `--pane`).

| Mode | Output |
|---|---|
| `outline` | Compact semantic tree with `@eN` refs (default) |
| `text` | Visible cells flattened to a plain UTF-8 string |
| `cells` | RLE cell-grid of the screen |
| `adapter` | Adapter-specific tree (vim's mode + filename, shell's state, …) |
| `hybrid` | All three concatenated |

`--png <path>` rasterizes the pane into a PNG (e.g. for inspection).
`--annotate` overlays numeric labels keyed to `@eN` refs.

`--keep-color` (with `--mode text` or `hybrid`) reconstructs per-cell
SGR escape sequences from each cell's color/attributes instead of
stripping them — useful when presenting output to a human or
debugging color logic. Ignored by non-text modes. Default text mode
stays plain (no escapes).

### `press`

```
agent-tui press "<key-tokens>"
```

Press a key-token sequence at the focused pane. Tokens use vim-style
notation: `<cr>`, `<esc>`, `<tab>`, `<f1>`..`<f12>`, `<c-x>`,
`<m-x>`, `<s-x>` for modifiers, `<bs>` backspace, etc. Plain text
between tokens is typed literally.

### `type`

```
agent-tui type "<text>"
```

Type literal text. No key-notation parsing — `<cr>` types `<cr>`
not a newline. Use `press` for keys, `type` for literal text, and
`send-ansi` for escape sequences.

### `send-ansi`

```
agent-tui send-ansi "<bytes>"
```

Send raw bytes to the PTY. Useful for slash-search in less,
mode-switching in apps that bypass readline, etc.

### `stdin`

```
agent-tui stdin [--text <text> | --bytes-hex <hex>]
```

Write bytes to the child's stdin **pipe**. Only works for panes
spawned with `--stdin pipe`. For PTY-stdin panes, use `type` /
`press` / `send-ansi` (those write through the master PTY).

### `close-stdin`

```
agent-tui close-stdin
```

Close the child's stdin pipe — EOF to the child's `read(stdin)`.
No-op for non-pipe panes.

### `tail`

```
agent-tui tail [--since <byte_offset>] [--strip-ansi]
```

Return raw bytes the child wrote to stdout+stderr. `--since N`
returns only bytes after the Nth observed byte (cumulative).
`--strip-ansi` removes CSI/SGR/OSC escape sequences and returns
plain UTF-8 text. Response includes `next_since` so callers can
poll for new bytes between snapshots.

### `resize`

```
agent-tui resize <cols> <rows>
```

Resize the focused pane. Emits `SIGWINCH` to the child process.

### `signal`

```
agent-tui signal <NAME>
```

Send a signal to the pane's child process group. Common values:
`INT` (Ctrl-C equivalent), `TERM` (graceful shutdown), `KILL`
(unconditional).

### `die`

```
agent-tui die [--pane <id>] [--grace [<ms>]]
```

Close the focused pane (or the named one) with **group-aware** teardown:
the signal goes to the child's process group, not just the child PID, so
a harness's forked MCP servers / tool subprocesses are reaped instead of
orphaned.

- Plain `die` sends a single SIGTERM to the group and returns immediately
  (no wait).
- `die --grace <ms>` sends SIGTERM to the group, polls for exit up to
  `<ms>`, then SIGKILL the group if anything is still alive. Given without
  a value (`--grace`), the window defaults to 3000 ms.

### `pane`

```
agent-tui pane focus <id>
agent-tui pane list
```

Pane focus management. Focus has three states (Auto / Focused /
Held); see [snapshot-refs.md](snapshot-refs.md).

### `wait`

```
agent-tui wait [--text <regex>] [--ref <selector> [--gone]] [--hash <hex>]
               [--since <n>] [--sequence <n>] [--idle <ms>]
               [--cursor-stable <ms>] [--alt-screen <bool>] [--exit]
               [--max <ms>] [--pane <id>]
```

Block until a state-change condition matches. Exactly one wait mode:
`--text` (regex over the rendered screen), `--ref` (a selector matches
a node; add `--gone` to wait until it matches nothing), `--hash` (screen
hash differs), `--since <n>` (event sequence passes `<n>`; **`--sequence`
is a visible alias**), `--idle <ms>` (no output for N ms),
`--cursor-stable <ms>`, `--alt-screen <bool>` (alt-screen toggled), or
`--exit` (child process exits). `--max <ms>` caps the wait (default
25000). See [wait-and-events.md](wait-and-events.md).

### `daemon`

```
agent-tui daemon run [--monitor-parent <PID>] [--idle-timeout-secs <SECS>]
agent-tui daemon status
agent-tui daemon shutdown [--all]
```

Daemon management. `run` is the in-process daemon (normally lazily
spawned by other commands; only used directly by tests and the
`AGENT_TUI_NO_LAZY_SPAWN=1` mode).

### `session`

```
agent-tui session gc [--older-than-days <DAYS>] [--all] [--dry-run]
```

`agent-tui session gc` reaps the on-disk state left behind by dead
sessions — sidecar files in the socket root plus cast dirs under
`$XDG_STATE_HOME/agent-tui/<session>/`. A crash or `kill -9` orphans
these; nothing else prunes them.

A session is **never** reaped while its daemon still answers the
socket (gc probes liveness without spawning a daemon). Among the dead,
`--older-than-days <DAYS>` (default 7) keeps anything whose most-recent
state is newer than the threshold, so a session that just crashed
isn't pulled out from under a retry. `--all` reaps every dead session
regardless of age. `--dry-run` reports what would be pruned without
deleting. `--json` emits `{pruned, skipped_alive, skipped_young,
dry_run}`.

### `doctor`

```
agent-tui doctor [--json]
```

Environment / sanity / version-drift diagnostics.

### `mcp`

```
agent-tui mcp serve
```

Run the MCP protocol over stdio (JSON-RPC: `initialize` →
`notifications/initialized` → `tools/list` → `tools/call`). Exposes a
**subset** of the CLI surface as MCP tools — these nine:

```
spawn  list  snapshot  press  type  wait  die  focus  daemon_status
```

Notably **not** exposed over MCP (use the CLI for these): `run`/`ask`
(subprocess-as-data sugar), `tail` (raw byte stream), `stdin`/
`close-stdin` (pipe feeding), `send-ansi`, `resize`, `signal`, `edit`/
`watch`, and `snapshot --png/--annotate`. If you need a headless CLI's
stdout, run it via the CLI `run` verb, not MCP.

**`snapshot` modes over MCP:** `outline` (default), `text`, `cells`,
`adapter`, `hybrid` — full parity with the CLI, including `text` (the
plain-string "what does the screen say" mode). `select`/`all` are also
supported. An unknown mode returns JSON-RPC `-32602` naming the valid
set.

**Shared daemon / focus:** the MCP server talks to the *same*
per-session daemon as the CLI — panes spawned via the CLI or a prior
MCP call persist. When more than one pane exists, the daemon can't
guess which you mean: tool calls return `NO_ACTIVE_PANE` (numeric
`1005`) until you either pass `pane` on the call or set focus with the
`focus` tool (`{"name":"focus","arguments":{"pane":"p1"}}`). A
single-pane session needs no focus call.

### `skills`

```
agent-tui skills list
agent-tui skills get <name> [--full]
```

Print embedded skill docs.

## Exit codes
<!-- tested-by: untested (no integration scenario exercises specific exit codes; CLI returns 1 on most errors today) -->

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | Generic error (see stderr) |
| `2` | Argument / usage error |
| `3` | Daemon unreachable |
| `4` | Policy denied (allowlist / governance) |

## Environment variables
<!-- tested-by: navigation -->

| Variable | What |
|---|---|
| `AGENT_TUI_SESSION` | Default `--session` value |
| `AGENT_TUI_SOCKET_DIR` | Default `--socket-dir` value |
| `AGENT_TUI_ALLOWED_BINARIES` | Default `--allowed-binaries` value |
| `AGENT_TUI_NO_LAZY_SPAWN` | If `1`, don't auto-spawn the daemon |
| `AGENT_TUI_MONITOR_PARENT_PID` | Daemon parent-watch target (zombie prevention) |
| `AGENT_TUI_IDLE_TIMEOUT` | Daemon idle-shutdown timeout (seconds) |
| `XDG_STATE_HOME` | Recorder + cast file root |
| `XDG_RUNTIME_DIR` | Socket dir default (`<dir>/agent-tui/<session>.sock`) |
