# agent-tui — Tracker

Working knowledge that doesn't belong in source comments: decisions, follow-ups, open questions, deferred work. Source rots; this file is where the WHY lives.

## Current phase

**Deferred-work sweep complete.** P0a + P0b + P0 close + P1 + P2 partial all
shipped on PR #1. The sweep cycle added focus tracking, marker/checkpoint
recorder events, first-bytes adapter re-detection, and an OSC 133 raw-byte
parser feeding the classifier.

Ready for the next major phase: **P3 (governance, auth vault, OPA-WASM,
per-pane mpsc queue)** or **P4 (MCP server + cargo-dist + distribution
channels)**.

## Substrate decision

**Default engine: `alacritty_terminal v0.26.0`.** Flipped from `wezterm-term` during P0a because `wezterm-term` is not published on crates.io — only available as a git dependency, which would block the `cargo install agent-tui` / `npm install -g` / `brew install` distribution promise documented in ADR-TA-012.

Implication: v1 does not have Kitty graphics + Sixel + OSC 8 hyperlinks in the engine. The PNG rasterizer (P1 task) will stub those. The `wezterm` crate stays as a placeholder; revisit when a published source appears or when `libghostty-vt` tags stable.

## Decisions worth remembering

| Topic | Decision | Rationale |
|---|---|---|
| Engine sync | `Mutex<Box<dyn MasterPty + Send>>` — wrap every portable-pty handle in a Mutex | Trait bound is `Send` only; storing the Pane in `Arc<>` needs Sync |
| Per-pane queue | Deferred from P0 to P1 cycle 13 | Mutex on `write_input` is functionally equivalent until the wait subsystem needs concurrent observers |
| Doctor | Wired to `DaemonStatus`, not its own command | Avoids a wire-protocol change; status payload covers reachability + version + pane count |
| Snapshot generation | `GenerationTracker` keyed by `PaneId` lives in `handlers/snapshot.rs` | Moves onto `Pane` itself when the per-pane queue lands |
| Signal delivery | `nix::sys::signal::killpg` to the process group via `MasterPty::process_group_leader` | Reaches forwarded shell children, not just immediate child |

## Deferred work

### Still deferred (justified)

- **task 0.4** — Per-pane `mpsc::Sender<Command>` queue. Re-deferred to P3 with policy. The only thing a real queue adds today is `PANE_BUSY` semantics under `--no-wait`, and no flow triggers that yet.
- **PluginAdapter (sub-process JSON-RPC over stdio)** — Moved to P4 alongside `mcp serve`. Both speak stdio JSON-RPC; build the framework once.
- **nvim / tmux built-in adapters** — Will land as external plug-ins via PluginAdapter once #2 ships. Avoids dragging `nvim --headless` / `tmux -CC` into CI.
- **wezterm engine real impl** — Blocked on `wezterm-term` being published to crates.io. Track only.

### Recently paid down (this PR)

- ✅ **Focus tracking** — `pane focus <id>` command + tri-state focus (Auto/Focused/Held). Focused-pane death promotes to Held; explicit refocus required.
- ✅ **Marker events** — Every command emits an `m` recorder event with {kind, ok, error_code}.
- ✅ **Checkpoint events** — Per-pane background task pushes `s` event every 1000 mutations.
- ✅ **First-bytes Detect re-attach** — PtyChild captures first 512 bytes; spawn fires a deferred re-detect that swaps the attached adapter if a better one wins on populated PaneInfo.
- ✅ **OSC 133 raw-byte parser** — Pre-engine scanner upgrades `PaneState::Unknown` → `Shell` / `Running` on FinalTerm shell-integration markers.
- ✅ **Stale comment sweep** — Removed every `lands in P0b` / `v0.1.0 wires only outline` lie.
- **Snapshot `--mode cells/hybrid/adapter`** — Only `--mode outline` wired in P0a. Cycle 15.
- **State classifier** — Returns `Unknown` unless alt-screen is on. Cycle 16. The 9-state heuristic stack ships with the recorder.
- **Asciicast recorder** — Event types exist (`agent-tui-recorder`); no writer yet. Cycle 17–18.
- **Stale doc comments** — CLI flag claims "alacritty lands in P5" (it's the default now); `server.rs` calls dispatch a "stub matrix" (it isn't anymore). Sweep in cycle 12.
- **CLI engine default** — Still `Wezterm` despite the substrate flip; spawn handler ignores it and instantiates `AlacrittyEngine` regardless. Cycle 12.
- **wezterm engine crate** — Returns the placeholder. Keep as a stub until either wezterm-term is published or libghostty-vt tags. Don't delete — the abstraction validates the trait works for multiple substrates.

## Open questions

- **OSC 133 detection precedence.** Once the classifier lands, does an OSC 133 marker in scrollback override a current alt-screen flag? Probably yes (shell-in-screen is still shell), but worth a test.
- **Wait under per-pane queue.** Does `wait --idle` block the queue from accepting concurrent reads? Current design: `wait` should NOT be on the input queue; it subscribes to engine events directly and the snapshot path is already lock-free.
- **Hash window size.** RFC pins 256. We can revisit if `WAIT_HASH_UNKNOWN` shows up in practice — the snapshot-then-wait-on-hash pattern depends on the window covering a typical agent action latency (~2-5 seconds of mutations).
- **Recorder backpressure.** RFC says "drop oldest non-g/s on full". Current sketch: bounded mpsc with `try_send`; on full, drop and bump `agent_tui_recorder_dropped_total`. Need to confirm tokio's broadcast channel doesn't already do this — it's the closer-fit primitive.

## Recent activity

| Commit | Phase | Notes |
|---|---|---|
| (next) | Deferred-work sweep | Focus tracking, marker/checkpoint events, first-bytes redetect, OSC 133 parser, stale-comment sweep |
| fcf460c | P2 partial | AdapterRegistry, sectioned generic outline, claude-code + shell built-ins |
| e212d9c | P1 | wait subsystem, cells/hybrid snapshots, classifier, recorder + rotation/retention |
| d7057f7 | P0 close | doctor wired to DaemonStatus |
| 3b2239f | P0b | keymap, press/type quiesce barrier, send_ansi, resize, signal |
| 2140ae9 | P0a | real engine + PTY + registry + spawn/die/list/snapshot |
| 04a664a | scaffolding | workspace + RFC + skeleton |
