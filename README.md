# agent-tui

Headless terminal-automation CLI for AI agents. Fast native Rust binary.

Drives any PTY app — `claude -p`, `vim`, `htop`, `lazygit`, `opencode`,
`gh api`, `jq`, … — through one verb-driven CLI. The pattern Vercel Labs'
[`agent-browser`](https://github.com/vercel-labs/agent-browser) uses for web
pages, adapted to terminals.

## Installation

Releases ship pre-built binaries for macOS (aarch64 + x86_64), Linux
(aarch64 + x86_64, both glibc and musl), and Windows (x86_64). See
the [latest release](https://github.com/ConductorOne/agent-tui/releases/latest)
for archives and checksums.

### Shell installer (macOS / Linux)

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/ConductorOne/agent-tui/releases/latest/download/agent-tui-installer.sh | sh
```

Pin to a specific version by swapping `latest` for the tag — e.g.
`download/v0.1.0/`.

### PowerShell installer (Windows)

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/ConductorOne/agent-tui/releases/latest/download/agent-tui-installer.ps1 | iex"
```

### Direct download

Pick the right archive for your platform from the
[release page](https://github.com/ConductorOne/agent-tui/releases/latest)
and verify it against `sha256.sum` before extracting.

### From source

```bash
git clone https://github.com/ConductorOne/agent-tui
cd agent-tui
cargo build --release
./target/release/agent-tui --help
```

### Requirements

- **Rust** — only when building from source ([rustup](https://rustup.rs))
- **Linux or macOS** — Windows port follows `docs/windows-strategy.md`
- **bash** — used by some `ask` recipes that need shell composition

## Quick Start

### Two modes — pick the verb for the shape of your task

**Mode A — subprocess as data.** The child has a non-interactive mode
(`claude -p`, `gh api`, `jq`, `gpg`). You want stdin in, stdout out.

```bash
agent-tui run --stdin "what is 40+2" -- claude -p
# → 42

agent-tui run --stdin '{"a":[1,2,3]}' -- jq -r '.a | length'
# → 3

agent-tui run --cwd /repo --env "BUILD=ci" -- ./build.sh
```

**Mode B — interactive driving.** The child has no headless mode
(`vim`, `htop`, `lazygit`). You spawn, snapshot, press, wait.

```bash
agent-tui spawn -- vim notes.md
agent-tui wait --text "notes.md"
agent-tui snapshot --mode outline
agent-tui press "i hello<esc>:w<cr>"
agent-tui wait --text "written"
agent-tui die
```

Each snapshot returns a compact outline keyed by `@eN` refs that an agent
acts on directly:

```
@e1 [tabline]    "main.go  utils.go*  README.md"
@e2 [statusline] "main.go [+]  Line 47 of 200  --INSERT--"
@e3 [buffer]     focused, 200 lines
@e4 [cmdline]    ""
```

## Commands

### Intent verbs (start here)

```bash
agent-tui run [--stdin TEXT] [--stdin-file PATH] [--cwd PATH] [--env K=V]
              [--max MS] [--raw] [--keep-daemon] -- argv...
# Spawn-pipe → stdin → close-stdin → wait-exit → tail → die. Returns
# {exit_code, stdout, elapsed_ms} as text or JSON (--json).

agent-tui ask <provider> "<prompt>"          # claude / codex / opencode / pi
agent-tui ask claude "what is 40+2"          # → 42

agent-tui edit <path> [--editor X]           # opens $EDITOR; returns file content
agent-tui watch -- argv...                   # spawn + tail --follow
```

### PTY primitives (Mode B)

```bash
agent-tui spawn [--stdin {pty|pipe|closed}] [--cwd PATH] [--env K=V] -- argv...
agent-tui press "<keys>"                     # vim-style: "i hello<esc>:w<cr>"
agent-tui type "<text>"                      # literal text (no key parsing)
agent-tui send-ansi "<bytes>"                # raw bytes for slash-search etc.
agent-tui resize <cols> <rows>
agent-tui signal <NAME>                      # INT / TERM / KILL
agent-tui die                                # close the focused pane
agent-tui pane focus <id>                    # multi-pane focus
agent-tui pane list
```

### Reading state

```bash
agent-tui snapshot --mode outline            # @eN tree (default)
agent-tui snapshot --mode text               # visible cells as plain UTF-8 string
agent-tui snapshot --mode cells              # RLE cell grid (engine-correctness)
agent-tui snapshot --mode adapter            # adapter-specific structure
agent-tui snapshot --mode hybrid             # all of the above
```

### Stdin pipes (for headless CLIs)

```bash
agent-tui stdin --text "<bytes>"             # write to the child's stdin pipe
agent-tui stdin --bytes-hex "68690a"
agent-tui close-stdin                        # EOF the child's stdin
```

### Output streaming

```bash
agent-tui tail                               # raw bytes the child wrote so far
agent-tui tail --strip-ansi                  # plain UTF-8 text
agent-tui tail --since 1024                  # only bytes after offset N
agent-tui tail --follow --strip-ansi         # stream live to stdout
agent-tui --json tail --follow               # one NDJSON envelope per chunk
```

### Waiting

```bash
agent-tui wait --text "<regex>"              # until regex matches the screen
agent-tui wait --hash <hex>                  # until screen hash != <hex>
agent-tui wait --sequence <n>                # until event >= n
agent-tui wait --idle <ms>                   # last resort — quiet period
agent-tui wait --exit                        # block until the child exits
                                             # response includes exit_code
agent-tui wait --alt-screen on|off
agent-tui wait --cursor-stable <ms>
```

### Recording & replay

```bash
# Every spawn writes an asciicast-v3 trace to
#   $XDG_STATE_HOME/agent-tui/<session>/<pane>.cast
agent-tui replay <cast> --mode text                     # re-run through fresh engine
agent-tui replay <cast> --expect-snapshot expected.json # regression: exit 1 on diff
```

### Skills (built-in docs)

```bash
agent-tui skills list                        # all skills bundled in this binary
agent-tui skills get core                    # main usage guide
agent-tui skills get core --full             # + references + templates
agent-tui skills get intent                  # verbs by what-you're-doing
agent-tui skills get ai-cli                  # driving claude/opencode/pi
```

### Diagnostics

```bash
agent-tui doctor                             # env / version / sanity check
agent-tui doctor --json
agent-tui list                               # panes in this session
agent-tui daemon status
agent-tui daemon shutdown                    # tear down this session's daemon
```

## Snapshot modes

| Mode | Token cost | Use it when |
|---|---|---|
| `outline` | ~200-400 | Default. Adapter-aware semantic tree with `@eN` refs |
| `text` | ~500-1500 | Plain UTF-8 string of visible cells. Easiest for ad-hoc parsing |
| `cells` | ~2000-5000 | RLE cell grid with SGR colors. Engine-correctness tests |
| `adapter` | ~50-200 | Just the adapter's app-specific structure (vim mode, shell state) |
| `hybrid` | ~3000+ | All four — debugging |

## Adapter manifests (drop-in TOML)

agent-tui adapters are how snapshots get semantic structure (lazygit's panels,
vim's modeline, etc.). Adding coverage for a new TUI app doesn't require Rust
code — drop a TOML file into the user adapter dir:

```bash
mkdir -p ~/.config/agent-tui/adapters
cat > ~/.config/agent-tui/adapters/k9s.toml <<'EOF'
name = "k9s"

[detect]
argv0 = ["k9s"]

[[regions]]
name = "header"
role = "status-bar"
rows = [0, 4]

[[regions]]
name = "table"
role = "table"
rows = [5, -2]

[[regions]]
name = "footer"
role = "footer"
rows = [-1, -1]
EOF

agent-tui daemon shutdown                    # respawn picks up the manifest
agent-tui spawn -- k9s
agent-tui snapshot --mode outline            # adapter: k9s
```

Lookup order: `$AGENT_TUI_ADAPTERS_DIR` → `$XDG_CONFIG_HOME/agent-tui/adapters/`
→ `~/.config/agent-tui/adapters/`. User drop-ins override bundled manifests
with the same `name`. Bundled today: `lazygit`, `tig`, `htop`, `less`, `fzf`,
`top`. Run `agent-tui skills get core --full` for the full schema.

## Recipes for `ask` (drop-in TOML)

`agent-tui ask <provider>` is sugar over `run` driven by per-CLI TOML
recipes. Same drop-in pattern as adapters; bundled recipes ship for
`claude`, `codex`, `opencode`, `pi`. Adding a new provider:

```bash
mkdir -p ~/.config/agent-tui/recipes
cat > ~/.config/agent-tui/recipes/aider.toml <<'EOF'
argv = ["aider", "--message"]
default_max_ms = 60_000
# Optional: pluck the answer out of verbose output.
# extract_after_line = "assistant"
# extract_until_line = "---"
EOF

agent-tui ask aider "fix the failing test"
```

Schema:

| Field | Required | Effect |
|---|---|---|
| `argv` | yes | The child argv |
| `default_max_ms` | no | `--max` default when caller doesn't override |
| `wrap_in_bash_cat` | no | Runs as `bash -c "cat \| <argv...>"` (for CLIs that refuse a daemon-supplied pipe stdin) |
| `extract_after_line` + `extract_until_line` | no | Plucks the answer between these marker lines |

## MCP (Claude Desktop / Claude Code)

`agent-tui mcp serve` is an MCP server. Drop into your
`claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "agent-tui": {
      "command": "/path/to/agent-tui",
      "args": ["mcp", "serve"]
    }
  }
}
```

Claude sees tools matching the CLI surface (`spawn`, `press`, `snapshot`,
`wait`, `die`, …) and can drive any TUI app on your machine.

Protocol: JSON-RPC 2.0 over stdio (MCP 2024-11-05). Any MCP-speaking
client works — Claude Desktop, Claude Code, your own.

## Sessions

Every agent-tui invocation runs against a session. Default is `default`;
override with `--session foo` or `$AGENT_TUI_SESSION=foo`. Sessions are
isolated daemons — useful for testing multi-agent flows or parallel
work:

```bash
agent-tui --session alice spawn -- vim alice.md
agent-tui --session bob   spawn -- vim bob.md
agent-tui --session alice press "iAlice<esc>:w<cr>"
```

State per session lives at `$XDG_STATE_HOME/agent-tui/<session>/`
(cast files, command-log, sidecars). `agent-tui daemon shutdown` ends
the current session; the idle timeout (5 min default) cleans up
abandoned ones.

## Global flags

```bash
--session <NAME>              # AGENT_TUI_SESSION
--socket-dir <DIR>            # AGENT_TUI_SOCKET_DIR
--engine <alacritty|wezterm>  # default: alacritty
--json                        # structured output for agent consumers
--timeout <MS>                # per-command timeout
--allowed-binaries <CSV>      # AGENT_TUI_ALLOWED_BINARIES
--content-boundaries          # wrap snapshots in nonced markers
```

## What's in the repo

```
crates/
  agent-tui              binary (CLI parsing + dispatch)
  agent-tui-protocol     wire types: CLI ↔ daemon JSON-RPC
  agent-tui-engine       headless VT engine trait
  agent-tui-engine-alacritty   alacritty_terminal-backed (default)
  agent-tui-engine-wezterm     wezterm-term placeholder
  agent-tui-daemon       per-session daemon + socket server
  agent-tui-recorder     asciicast-v3-extended event log
  agent-tui-adapter      adapter trait + TOML manifests
  agent-tui-integration  bwrap + docker integration tests
  xtask                  cargo xtask: docs/cli/cross-platform coverage

docs/
  RFC.md                 canonical architecture RFC
  ux-rfc.md              UX surface RFC (run, tail, manifests, …)
  ux-rfc-followups.md    phased follow-on plan
  skills-rfc.md          skills system design
  gaps-and-emergent.md   what we learned building it
  windows-strategy.md    Windows port plan

scripts/                 example shell scripts (ask-claude, watch-build, …)
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Pre-push:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask cross-check           # macOS + Windows compile
cargo xtask docs-coverage         # skill ↔ test correspondence
cargo xtask cli-coverage          # CLI surface ↔ skill docs
```

## License

Apache-2.0. See [`LICENSE`](LICENSE).
