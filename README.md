# agent-tui

[![Release](https://img.shields.io/github/v/release/ConductorOne/agent-tui)](https://github.com/ConductorOne/agent-tui/releases/latest)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

Headless terminal automation for AI agents — the
[`agent-browser`](https://github.com/vercel-labs/agent-browser) pattern, for
PTYs. Drive any terminal app (`vim`, `htop`, `psql`, a REPL, `claude`) and
read its screen back as structured, addressable text.

![agent-tui reads htop as structured terminal text](docs/assets/agent-tui-hero.gif)

## Read any screen as text

`htop` is a full-screen ncurses dashboard — no headless mode, no `--json`.
agent-tui spawns it on a PTY and returns its live screen as plain text:

```bash
agent-tui spawn -- htop
agent-tui wait --idle 500
agent-tui --json snapshot --mode text | jq -r .data.text
```

```text
    0[         0.0%]   4[         0.0%]   8[          0.0%] 12[          0.0%]
    1[*******100.0%]   5[         0.0%]   9[          0.0%] 13[          0.0%]
  Mem[|||||#*@@@@@@@@@@@@@@@16.0G/124G] Tasks: 29, 117 thr, 0 kthr; 7 running
  Swp[                           0K/0K] Load average: 0.68 0.82 0.69

    PID USER       PRI  NI  VIRT   RES   SHR S  CPU%-MEM%   TIME+  Command
  50386 alice      20   0  4900  3336  2456 R 160.0  0.0  0:00.02 htop
      1 root       20   0 3831M  274M 38368 S   0.0  0.2  0:55.95 systemd
```

No screen-scraping, no terminal-byte parsing, no coordinates.

## Two ways to drive a child

agent-tui drives a child two ways — pick the verb for your task:

- **`run` — subprocess as data.** The child has a non-interactive mode
  (`claude -p`, `gh api`, `jq`, `gpg`). You want stdin in, stdout out, in one
  shot. No screen, no loop:

  ```bash
  agent-tui run --stdin '{"a":[1,2,3]}' -- jq -r '.a | length'   # → 3
  ```

- **`spawn` — drive a live screen.** The child is interactive (`vim`, `htop`,
  `psql`) or you want to observe its TUI. You step it: spawn the PTY, read the
  snapshot, act on refs, wait for the next state (the rest of this README).

In short: **want stdout? → `run`. Want to observe or drive a screen?
→ `spawn` + `snapshot` + `press` + `wait`.** Full mapping in
[How it works](#how-it-works--the-agent-browser-bridge).

## Install

Pre-built binaries ship on **[GitHub Releases](https://github.com/ConductorOne/agent-tui/releases/latest)**
for macOS (aarch64 + x86_64), Linux (aarch64 + x86_64, glibc and musl), and
Windows (x86_64).

```bash
# macOS / Linux
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/ConductorOne/agent-tui/releases/latest/download/agent-tui-installer.sh | sh
```

```powershell
# Windows
powershell -ExecutionPolicy Bypass -c "irm https://github.com/ConductorOne/agent-tui/releases/latest/download/agent-tui-installer.ps1 | iex"
```

<details>
<summary>Direct download + verify, or build from source</summary>

**Direct download + verify** — grab the archive for your platform and check it
against `sha256.sum` before extracting:

```bash
base=https://github.com/ConductorOne/agent-tui/releases/latest/download
curl -LO "$base/agent-tui-aarch64-unknown-linux-gnu.tar.xz"
curl -LO "$base/agent-tui-aarch64-unknown-linux-gnu.tar.xz.sha256"
sha256sum -c agent-tui-aarch64-unknown-linux-gnu.tar.xz.sha256
tar -xf agent-tui-aarch64-unknown-linux-gnu.tar.xz
```

Pin a version by swapping `latest` for a tag — e.g. `download/v0.1.0/`.

**Docker / GHCR** — a binary-only multi-arch image (linux/amd64 + linux/arm64)
is published to the GitHub Container Registry. Run it directly, or copy the
binary into your own image:

```bash
docker run --rm ghcr.io/conductorone/agent-tui --help
```

```dockerfile
COPY --from=ghcr.io/conductorone/agent-tui /usr/local/bin/agent-tui /usr/local/bin/agent-tui
```

**`cargo install`** — not yet published to crates.io. Build from source for now
(below).

**From source:**

```bash
git clone https://github.com/ConductorOne/agent-tui
cd agent-tui
cargo build --release
./target/release/agent-tui --help
```

</details>

## Refs: bringing the DOM to the terminal

Every snapshot turns the screen into a tree of **addressable nodes**, each with
a stable **ref**: a handle the agent uses to *name* an on-screen element
instead of guessing where it is.

```bash
agent-tui spawn -- vim notes.md
agent-tui wait --ref '@vim.buffer'        # vim has rendered
agent-tui --json snapshot --select '@vim.mode' | jq -c '.data.outline.nodes[0]'
```

```json
{"durable":true,"ref":"@vim.mode","role":"mode","value":"normal"}
```

`@vim.mode` is a **durable ref** — the same handle points at vim's mode every
frame, whether it reads `normal` or `insert`. Adapter-aware apps emit named
refs (`@vim.buffer`, `@vim.mode`, `@shell.prompt`); anything without a
dedicated adapter still gets generic positional refs (`@e1`, `@e2`) tagged with
a `role`. This mirrors [`agent-browser`](https://github.com/vercel-labs/agent-browser)'s
element refs, applied to the terminal screen — see the
[mapping below](#how-it-works--the-agent-browser-bridge).

You find and wait on refs with **selectors** — a CSS subset:
`[role=buffer][focused]`, `@vim.mode[value=insert]`, `@tmux.pane[%2]`.

## Refs in action

This edit flow uses no `sleep`: every step blocks on a *structured state*, so
it's deterministic regardless of how fast vim is:

```bash
agent-tui spawn -- vim todo.txt
agent-tui wait --ref '@vim.buffer'              # 1. wait for the buffer to exist
agent-tui press i                               # 2. enter insert mode
agent-tui wait --ref '@vim.mode[value=insert]'  # 3. wait for the mode to flip
agent-tui type 'review the draft'               # 4. type
agent-tui press '<esc>'                         # 5. leave insert
agent-tui wait --ref '@vim.mode[value=normal]'  # 6. wait for the mode to flip back
agent-tui press ':wq<cr>'                       # 7. save + quit
```

The mode ref flips the instant vim is ready, so the agent acts on a fact rather
than a timer:

```bash
agent-tui --json snapshot --select '@vim.mode' | jq -c '.data.outline.nodes[0]'
# before `press i`        → {"durable":true,"ref":"@vim.mode","role":"mode","value":"normal"}
# after  `wait …=insert`  → {"durable":true,"ref":"@vim.mode","role":"mode","value":"insert"}
```

`wait --ref` won't false-fire on text you've typed-but-not-yet-executed the way
a screen regex can, and `--gone` inverts it — `wait --ref '@vim.cmdline[focused]' --gone`
blocks until a prompt *closes*.

## How it works — the agent-browser bridge

agent-tui is one CLI plus a **per-session daemon** that owns the PTY and serves
state over a Unix socket. Each invocation is a thin client: it connects, runs
one verb, prints the result. The daemon keeps the terminal alive between calls.

If you've used agent-browser, the model maps over almost directly:

| agent-browser (web) | agent-tui (terminal) |
|---|---|
| browser context / window | session (`--session`) |
| page / tab | PTY pane (`p1`) |
| DOM tree | snapshot `outline` — semantic node tree |
| accessibility / element refs | `@refs` (`@vim.buffer`, `@e1`) |
| CSS selectors | selector grammar (`[role=buffer][focused]`) |
| `click` / `type` | `press` / `type` (route with `--to '<selector>'`) |
| `waitForSelector` | `wait --ref '<selector>'` (+ `--gone`) |
| `textContent` / `innerText` | `snapshot --mode text` / `--select` |
| `screenshot` | `snapshot --png screen.png` — rasterized PNG output, optionally with `--annotate` / `--chrome` |

The two ways to drive a child (above) are the terminal analogue of "fetch a URL"
vs "automate a page": **`run`** for stdout-shaped work, the
**`spawn`/`snapshot`/`press`/`wait`** loop for live screens.

## Key features

- **Structured snapshots** — read any screen as a semantic `outline` tree, a
  plain-`text` string, a raw `cells` grid, or the `adapter` view.
- **Refs + selectors** — stable handles to on-screen nodes, addressed with a
  CSS-subset selector grammar (`[role=…]`, `@app.node`, `--gone`).
- **Persistent per-session daemon** — one daemon owns the PTY; every CLI call
  is a fast round-trip over a Unix socket. Sessions isolate parallel work.
- **Deterministic `wait`** — block on a ref, a regex, a screen hash, an event
  sequence, child exit, or (last resort) idle. No `sleep`s.
- **asciicast record/replay** — every session is recorded to asciicast-v3;
  replay it through a fresh engine for ground-truth regression checks.
- **Pluggable adapters** — teach agent-tui a new TUI's structure with a drop-in
  TOML manifest; no Rust required.
- **MCP server** — `agent-tui mcp serve` exposes the whole surface as MCP tools
  so Claude (Desktop / Code) can drive any terminal app.

## More examples

**A headless API call — `gh api … --jq`.** `run` is the "subprocess as data"
verb: stdin in, stdout out, exit code, all in one shot.

```bash
agent-tui run -- gh api /repos/ConductorOne/agent-tui \
  --jq '{repo: .full_name, lang: .language, default_branch: .default_branch}'
```

```json
{
  "default_branch": "main",
  "lang": "Rust",
  "repo": "ConductorOne/agent-tui"
}
```

**A live REPL — `python3`.** Spawn it, type an expression, wait for the result
on screen, read it back:

```bash
agent-tui spawn -- python3 -q
agent-tui wait --text '>>>'
agent-tui type 'sum(range(101))'
agent-tui press '<cr>'
agent-tui wait --text '5050'
agent-tui --json snapshot --mode text | jq -r .data.text
```

```text
>>> sum(range(101))
5050
>>>
```

**A one-shot `vim` edit.** Open, prepend to line 1, leave insert mode, then read
the buffer back — the `wait` on `value=normal` confirms the edit landed before
the snapshot:

```bash
agent-tui spawn -- vim notes.md
agent-tui wait --ref '@vim.buffer'
agent-tui press 'ggIHELLO <esc>'                # prepend to line 1, leave insert
agent-tui wait --ref '@vim.mode[value=normal]'  # the edit has landed
agent-tui --json snapshot --mode text | jq -r .data.text
```

```text
HELLO first line
second line
third line
```

**Snapshot a TUI's structure — `htop`.** The `outline` mode returns the
adapter's semantic regions instead of raw text:

```bash
agent-tui spawn -- htop
agent-tui wait --idle 500
agent-tui --json snapshot --mode outline | jq -c '[.data.outline.nodes[] | {ref, role}]'
```

```json
[{"ref":"@e1","role":"meters"},{"ref":"@e2","role":"table"},{"ref":"@e3","role":"footer"}]
```

**Ask an AI CLI — `claude -p`.** `ask` is sugar over `run` with per-provider
recipes (`claude`, `codex`, `opencode`, `pi`); `run --stdin … -- claude -p` is
the explicit form:

```bash
agent-tui ask claude "what is 40+2"
# → 42

agent-tui --json run --stdin "what is 40+2" -- claude -p
```

```json
{"argv":["claude","-p"],"exit_code":0,"stdout":"42\n","elapsed_ms":3311}
```

## Learn more

- **Quickstart** — [`docs/quickstart.md`](docs/quickstart.md) (install →
  `spawn` → `snapshot` → `wait`/`press` in a few minutes).
- **Adapters** — [`docs/adapters.md`](docs/adapters.md) (write your own TOML
  adapter). **MCP** — [`docs/mcp.md`](docs/mcp.md) (Claude Desktop / Claude
  Code / generic MCP client setup).
- **Design notes** — [`docs/design/`](docs/design/) holds the original design
  RFCs (architecture, UX, addressing, skills). These predate the public release
  and may not match current behavior.
- **Built-in skills** — the binary ships its own docs: `agent-tui skills list`,
  then `agent-tui skills get core --full` for the canonical guide (or
  `addressing`, `vim`, `shell`, `ai-cli`, `tui-apps`).
- **MCP setup** — add `{"command": "agent-tui", "args": ["mcp", "serve"]}` to
  your `claude_desktop_config.json` (or any MCP client). See
  `agent-tui skills get core` for the full tool list.
- **Contributing** — [CONTRIBUTING.md](CONTRIBUTING.md).
- **License** — [Apache-2.0](LICENSE).
