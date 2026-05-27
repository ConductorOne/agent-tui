# Snapshot and `@eN` Refs

Compact element references that let an AI agent interact with a TUI
in ~200-400 tokens instead of parsing raw terminal bytes.

**Related:** [commands.md](commands.md), [wait-and-events.md](wait-and-events.md), [../SKILL.md](../SKILL.md).

## Why refs
<!-- tested-by: navigation -->

Naïve approach:

```
Full terminal screen → AI parses ANSI/wcwidth/grid → coords → action
                                                    (~2000-5000 tokens, brittle)
```

`agent-tui` approach:

```
Compact semantic outline → @eN refs assigned → direct interaction
                          (~200-400 tokens, deterministic)
```

The outline is **semantic**: structural nodes from the program's
adapter (vim's modeline, fzf's prompt/list, htop's table) get refs;
purely decorative cells don't.

## The four snapshot modes
<!-- tested-by: bwrap_vim_opens_file_and_shows_content -->

| Mode | Token cost | Use it when |
|---|---|---|
| `outline` | ~200-400 | Default. Almost everything. |
| `cells` | ~2000-5000 | You need exact cell positions / wide chars / SGR colors |
| `adapter` | ~50-200 | Adapter has app-specific state (vim's mode, shell's prompt-vs-running) |
| `hybrid` | ~3000+ | Debugging — all three combined |

```bash
agent-tui snapshot --mode outline      # default
agent-tui snapshot --mode cells
agent-tui snapshot --mode adapter
agent-tui snapshot --mode hybrid
```

## Outline format
<!-- tested-by: bwrap_htop_renders_process_list_and_fkeys -->

```
Pane: p1
Engine: alacritty 0.26.0
Adapter: vim 9.0
Size: 80×24
Sequence: 142

@e1 [titlebar] "sample.txt + 3 lines"
@e2 [buffer]
  @e3 [line 1] "hello"
  @e4 [line 2] "world"
  @e5 [line 3] ""
@e6 [statusline] "INSERT -- sample.txt"
@e7 [cmdline] ""
```

Each refs `@eN` is unique within the snapshot. Refs are assigned in
deterministic outline-walk order — the same screen produces the same
ref numbers across snapshots IF nothing changed.

## Ref lifecycle
<!-- tested-by: vim_modified_file_marks_status_node -->

Refs are **stale the moment the pane changes.** That includes:

- Any `press`, `type`, `send-ansi` that produces output
- Window resize (rewrap can move things)
- An adapter promotion (e.g. `Unknown → Shell` after an OSC 133)
- Time passing in a running program (htop, less +F mode)

**Rule:** snapshot, then act, then re-snapshot. Don't carry refs
across actions.

## Adapter-durable IDs (P2)
<!-- tested-by: untested (adapter-durable id binding lands in P2; refs are single-snapshot until then) -->

For adapters that expose stable identifiers (vim buffer numbers,
tmux pane ids, terminal-multiplexer tab ids, …) the adapter SHOULD
attach a `data-aid="…"` style identifier to its nodes. When present,
the daemon uses the adapter id to re-bind refs across snapshots —
so a vim `@e3 buffer line 1` keeps its identity even if intermediate
lines shifted.

This is a **best-effort** durability mechanism. The default `generic`
adapter can't provide it, so generic-adapter refs are always
single-snapshot.

## Tested refs lifecycle
<!-- tested-by: navigation -->

Three integration tests assert ref stability under specific
mutations:

<!-- tested-by: vim_modified_file_marks_status_node -->
- `vim_modified_file_marks_status_node` — vim's `[+]` modified
  marker becomes part of the statusline node without renumbering
  surrounding refs.

<!-- tested-by: vim_insert_mode_shows_in_outline -->
- `vim_insert_mode_shows_in_outline` — entering INSERT mode promotes
  the statusline node text but doesn't move buffer-line refs.

<!-- tested-by: vim_command_mode_carries_command_line -->
- `vim_command_mode_carries_command_line` — `:`-prefixed command-line
  state is reflected in the cmdline node, not bolted onto statusline.

## Hashing
<!-- tested-by: bwrap_fzf_typed_filter_narrows_candidates -->

Every snapshot includes a `Sequence:` line + a hash of the rendered
cells. Both are stable across redundant repaints (e.g. cursor blink)
so `wait --hash <prev-hash>` doesn't false-positive on cosmetic
churn. See [wait-and-events.md](wait-and-events.md) for the sequence
number model.

## Annotated PNGs
<!-- tested-by: untested (PNG rasterizer stubbed; lands when alacritty engine gains a real rasterizer in P2) -->

For multimodal models, render with `--png <path> --annotate`:

```bash
agent-tui snapshot --mode outline --png pane.png --annotate
```

Each `[N]` label in the PNG maps to `@eN` in the outline. Useful for
hand-debugging or when you want a screenshot for a bug report.

## Content boundaries
<!-- tested-by: untested (snapshot.rs unit tests cover the nonce logic; no integration scenario yet exercises the CLI flag end-to-end) -->

When `--content-boundaries` is set, snapshot payloads are wrapped in
per-snapshot nonced markers:

```
<<<AGENT_TUI_OUTPUT_a3f1b29c>>>
… outline …
<<<END_a3f1b29c>>>
```

Defense-in-depth against a TUI emitting a colliding marker into its
own output. The nonce is fresh per snapshot. Pair with strict
parsing on the agent's side.
