# Quickstart

Install agent-tui, spawn a terminal program, read its screen, and drive it —
in about five minutes.

## Install

The fastest path is the release installer:

```bash
# macOS / Linux
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/ConductorOne/agent-tui/releases/latest/download/agent-tui-installer.sh | sh
```

`cargo install` is not yet published. To build from source instead:

```bash
git clone https://github.com/ConductorOne/agent-tui
cd agent-tui
cargo build --release
./target/release/agent-tui --help
```

Confirm the binary and its environment:

```bash
agent-tui --version
agent-tui doctor
```

`doctor` runs environment, sanity, and version-drift diagnostics and reports
whether the per-session daemon is reachable. Run it first if anything later
misbehaves; add `--diagnostic-bundle <path>` when you need a portable debug
bundle for an issue.

## The two ways to drive a program

- **Subprocess as data** — the program has a headless mode (`claude -p`,
  `gh api`, `jq`). You want stdin in, stdout and an exit code out. Use `run`.
- **Interactive driving** — the program is a TUI with no headless mode (vim,
  htop, less, lazygit) or you want to observe its screen. Use the
  `spawn` → `wait` → `snapshot` → `press` loop.

## Subprocess as data: `run`

```bash
agent-tui run --stdin "what is 40+2" -- claude -p

# JSON envelope for a calling agent
agent-tui --json run --stdin "what is 40+2" -- claude -p
# → {"argv":["claude","-p"],"exit_code":0,"stdout":"42\n","elapsed_ms":2064}
```

`run` bundles spawn + write-stdin + close-stdin + wait-for-exit +
strip-ANSI + cleanup, and shuts the per-session daemon down on return. Raise
the deadline with `--max <ms>` (default 60000) for slow commands.

## Interactive driving: spawn / wait / snapshot / press

```bash
agent-tui spawn -- vim notes.md       # 1. spawn a PTY-backed pane
agent-tui wait --text "notes.md"      # 2. wait until the screen is ready
agent-tui snapshot --mode outline     # 3. read the screen as a compact tree
agent-tui press "i hello<esc>:wq<cr>" # 4. act (insert, type, save, quit)
agent-tui die                         # 5. tear down
```

### Snapshot

`snapshot` defaults to `outline` — a compact, ref-bearing tree that costs a
few hundred tokens instead of a screen of raw terminal bytes. Other modes:

```bash
agent-tui snapshot --mode text        # visible cells as a plain string
agent-tui snapshot --mode cells       # RLE cell grid (exact positions, colors)
agent-tui snapshot --mode adapter     # adapter-specific structure
agent-tui snapshot --png screen.png   # rasterize the screen to a PNG
```

Use `outline` for most TUI apps and `text` when the pane is just unstructured
output.

### Wait

`wait` blocks on a screen-state condition instead of sleeping. Pick the most
specific form:

```bash
agent-tui wait --ref '[role=cmdline][focused]'  # a structured node appears
agent-tui wait --text "written"                 # a regex matches the screen
agent-tui wait --exit                            # the child process exits
agent-tui wait --idle 150                        # 150 ms with no changes
```

A timed-out `wait` exits non-zero with the screen state at exit; raise `--max`
(default 25000 ms) when the thing you wait for legitimately takes longer.

### Press and type

`press` parses vim-style key notation — `<cr>`, `<esc>`, `<c-c>`, `<f5>` —
and types plain text literally. `type` sends literal text with no key parsing.
`send-ansi` emits hex-encoded raw bytes for lower-level escape sequences.

```bash
agent-tui press ":q!<cr>"
agent-tui type "search term"
agent-tui press "/needle<cr>"
agent-tui send-ansi 2f6e6565646c650d        # bytes for /needle<CR>
```

## Sessions

Every command runs against a per-session daemon that owns the PTY. The default
session is `default`; isolate work with `--session <name>` or the
`AGENT_TUI_SESSION` environment variable. The daemon exits on idle timeout
(default 5 minutes), when its parent process tree dies, or on
`agent-tui daemon shutdown`.

## Next steps

- Built-in skills ship inside the binary: `agent-tui skills list`, then
  `agent-tui skills get core --full` for the canonical guide. Other skills
  cover `addressing` (the selector grammar), `shell`, `vim`, `ai-cli`, and
  `tui-apps`.
- [adapters.md](adapters.md) — teach agent-tui a new TUI app with a TOML
  adapter.
- [mcp.md](mcp.md) — drive panes from Claude Desktop, Claude Code, or any MCP
  client.
