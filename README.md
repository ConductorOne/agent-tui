# agent-tui

A headless terminal browser for LLM agents. Single static binary. Snapshot a
TUI's cell grid into a compact semantic outline, address elements by stable
`@eN` refs, drive interaction over a clean CLI — the same pattern Vercel Labs'
[`agent-browser`](https://github.com/vercel-labs/agent-browser) uses for web
pages, adapted to PTYs.

> **Status:** Scaffolding. The design is locked down in `docs/RFC.md`; code is
> the early P0 substrate work.

## Quick mental model

```bash
agent-tui spawn nvim                   # start nvim under agent-tui control
agent-tui snapshot                     # see what's on screen now
agent-tui press "i hello<esc>:w<cr>"   # write a file
agent-tui wait --idle 300              # block until quiescent
agent-tui snapshot                     # observe the result
agent-tui die                          # close the PTY session
```

Each `snapshot` returns a compact outline keyed by `@eN` refs that an agent
acts on directly:

```
Session: default  | Pane: p1 (nvim) | 80x24 | alt_screen | gen 17 | seq 4423

@e1 [tabline]    "main.go  utils.go*  README.md"
@e2 [statusline] "main.go [+]  Line 47 of 200  --INSERT--"
@e3 [buffer]     focused, 200 lines
@e4 [cmdline]    ""
```

For the full design, read [`docs/RFC.md`](docs/RFC.md).

## What's in the repo

```
crates/
  agent-tui              # the binary (CLI entry point)
  agent-tui-protocol     # JSON-RPC types for CLI ↔ daemon IPC
  agent-tui-engine       # the headless VT engine trait
  agent-tui-engine-wezterm    # default engine (wezterm-term-backed)
  agent-tui-engine-alacritty  # lean alternative engine
  agent-tui-daemon       # the long-lived per-session daemon
  agent-tui-recorder     # asciicast-v3-extended event log
  agent-tui-adapter      # adapter trait + plug-in IPC shim

docs/
  RFC.md                 # the production-ready architecture RFC
```

## Build

Requires [rustup](https://rustup.rs).

```bash
just build                # cargo build --workspace
just test                 # cargo nextest run if available, else cargo test
just check                # cargo check + clippy
just fmt                  # rustfmt
just run -- snapshot      # cargo run --bin agent-tui -- snapshot
```

Plain `cargo` works too — `just` is just convenience.

## Install (future)

When the binary is ready (post-P0):

```bash
npm install -g agent-tui            # downloads matching binary for your arch
brew install agent-tui              # Homebrew bottle
cargo install agent-tui             # source build via crates.io
```

See RFC §13 for the distribution matrix.

## License

Apache-2.0. See [`LICENSE`](LICENSE).
