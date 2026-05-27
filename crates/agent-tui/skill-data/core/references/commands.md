# Commands

Every `agent-tui` subcommand, flag, and alias. This file is meant to
be regeneratable from `agent-tui --help` — when the CLI changes,
update this file in the same PR (CI's `xtask docs-coverage` will
catch flags referenced in skill pages that don't exist in clap).

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
              [--raw] [--keep-daemon] -- <argv...>
```

Sugar verb: spawn + optionally write stdin + close-stdin + wait for
exit + tail bytes + die + shut daemon. The "subprocess as data"
pattern in one verb. Default `--stdin <text>` writes literal bytes
then closes; `--raw` returns bytes-with-escapes instead of stripped
text; `--keep-daemon` skips the daemon shutdown step.

### `spawn`

```
agent-tui spawn [--stdin pty|pipe|closed] [--cols <N>] [--rows <N>] -- <argv...>
```

Spawn a PTY-backed pane running `argv`. Returns the pane id (e.g.
`p1`). Honors the `--allowed-binaries` allowlist.

`--stdin pty` (default) gives the slave PTY for stdin. `--stdin
pipe` gives a kernel pipe — required by CLIs that do `isatty(0)`
checks. `--stdin closed` ties stdin to `/dev/null`.

### `list`

```
agent-tui list
```

List the panes in the current session.

### `snapshot`

```
agent-tui snapshot [--pane <id>] [--mode outline|cells|adapter|hybrid]
                   [--png <path>] [--annotate]
```

Snapshot the focused pane (or a specific one with `--pane`).

| Mode | Output |
|---|---|
| `outline` | Compact semantic tree with `@eN` refs (default) |
| `cells` | RLE cell-grid of the screen |
| `adapter` | Adapter-specific tree (vim's mode + filename, shell's state, …) |
| `hybrid` | All three concatenated |

`--png <path>` rasterizes the pane into a PNG (e.g. for inspection).
`--annotate` overlays numeric labels keyed to `@eN` refs.

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
agent-tui die [--pane <id>]
```

Close the focused pane (or the named one). Sends SIGTERM, waits a
bounded grace period, then SIGKILL if needed.

### `pane`

```
agent-tui pane focus <id>
agent-tui pane list
```

Pane focus management. Focus has three states (Auto / Focused /
Held); see [snapshot-refs.md](snapshot-refs.md).

### `wait`

```
agent-tui wait [--text <regex>] [--hash <hex>] [--sequence <n>] [--idle <ms>]
               [--max <ms>] [--pane <id>]
```

Block until a state-change condition matches. See
[wait-and-events.md](wait-and-events.md).

### `daemon`

```
agent-tui daemon run [--monitor-parent <PID>] [--idle-timeout <SECS>]
agent-tui daemon status
agent-tui daemon shutdown [--all]
```

Daemon management. `run` is the in-process daemon (normally lazily
spawned by other commands; only used directly by tests and the
`AGENT_TUI_NO_LAZY_SPAWN=1` mode).

### `doctor`

```
agent-tui doctor [--json]
```

Environment / sanity / version-drift diagnostics.

### `mcp`

```
agent-tui mcp serve
```

Run the MCP protocol over stdio. Exposes a subset of the CLI surface
as MCP tools.

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
