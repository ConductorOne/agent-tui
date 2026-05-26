# agent-tui — Tracker

Working knowledge that doesn't belong in source comments: decisions, follow-ups, open questions, deferred work. Source rots; this file is where the WHY lives.

## Current phase

**P1 — Observation & Wait** (in progress). P0a + P0b shipped: real engine, PTY, registry, spawn/die/list/snapshot/press/type/send_ansi/resize/signal/doctor wired end-to-end.

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

## Deferred work (paying down in P1)

- **task 0.4** — Per-pane `mpsc::Sender<Command>` queue. **Re-deferred to P3** after design review: snapshots are already lock-free, the engine broadcast is independent of the writer Mutex, and the wait subsystem doesn't need a queue to subscribe. The only thing a real queue adds is `PANE_BUSY` semantics under `--no-wait`, and no real agent flow triggers that today. Revisit when governance / policy gates need to reject in-flight writes.
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
| d7057f7 | P0 close | doctor wired to DaemonStatus |
| 3b2239f | P0b | keymap, press/type quiesce barrier, send_ansi, resize, signal |
| 2140ae9 | P0a | real engine + PTY + registry + spawn/die/list/snapshot |
| 04a664a | scaffolding | workspace + RFC + skeleton |
