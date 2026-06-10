# Waiting and Events

The `wait` subsystem is how an agent synchronizes its next action
with the pane's current state. Pick the most specific form for the
situation — `--idle` alone is the wrong default.

**Related:** [commands.md](commands.md), [snapshot-refs.md](snapshot-refs.md), [../SKILL.md](../SKILL.md).

## The four `wait` forms
<!-- tested-by: navigation -->

```bash
agent-tui wait --text "<regex>"        # until a regex matches the rendered screen
agent-tui wait --hash <hex>            # until the screen's hash != <hex>
agent-tui wait --since <n>             # until the event sequence passes <n> (alias: --sequence)
agent-tui wait --idle <ms>             # until <ms> ms pass with no PTY output
```

Auxiliary forms (rarely used directly):

```bash
agent-tui wait --alt-screen on|off     # alt-screen toggled (modal open/close)
agent-tui wait --cursor-stable <ms>    # cursor stopped moving for ms
agent-tui wait --exit                  # the child process exited
agent-tui wait --sequence <n>          # visible alias of --since
```

| Form | Strength | Right when |
|---|---|---|
| `--text` | Strong (exact match) | A program prints a known marker on success |
| `--hash` | Strong (exact diff) | Loop: "wait until the screen differs from this prior hash" |
| `--sequence` | Strong (event-ordered) | Sync with the recorder's event stream |
| `--idle` | Weak (heuristic) | Last resort; nothing else applies |

### Why `--idle` alone is a code smell
<!-- tested-by: bwrap_fzf_typed_filter_narrows_candidates -->

Idle waits are a heuristic — they tell you "the screen stopped
changing for N ms," not "the screen reached the state I care about."
A slow program can fool a short idle wait; a chatty program can foil
a long one.

Use `--idle` for **post-condition settling** after a stronger wait:

```bash {test=idle-post-condition}
agent-tui type "ban"
agent-tui wait --text "1/3"        # the strong wait — definitive
agent-tui wait --idle 150          # settle for the next snapshot
agent-tui snapshot
```

Not as the primary synchronization:

```bash
agent-tui type "ban"
agent-tui wait --idle 500          # ❌ what does "500ms" mean for THIS program?
agent-tui snapshot
```

## Push instead of poll: `agent-tui events`
<!-- tested-by: events_init_then_screen_changed_then_child_exited -->

`wait` blocks for ONE condition; `agent-tui events` streams EVERY
state change as NDJSON until the child exits. Use it when you're
driving a long-lived TUI (a harness, a REPL) and want to react to
each frame instead of re-running `snapshot` on a timer:

```bash
agent-tui events --pane p1 --debounce 200
```

emits `init` (baseline `screen_hash` + geometry + cursor + modes),
then `screen_changed` (throttled; only when the canonical hash
actually differs), `mode_changed` (alt-screen / bracketed-paste
flips), `bell`, and a terminal `child_exited` with the exit code.
See [commands.md](commands.md) for the per-event payloads. The
`screen_hash` it carries is the same canonical hash `snapshot`
returns, so a driver can `events` → notice a change → `snapshot`
for the full frame, with no race: the hashes line up exactly.

## The sequence-number stream
<!-- tested-by: untested (sequence-based wait is supported at the daemon level; no integration test asserts the cross-event ordering guarantee yet) -->

Every PTY event (input, output, repaint, resize, marker, checkpoint)
gets a monotonic `sequence` number assigned by the engine. Snapshots
include `Sequence: N` so subsequent waits can target "any change
after this point":

```bash
SEQ=$(agent-tui snapshot --json | jq -r .sequence)
agent-tui press "<f5>"             # toggle htop tree mode
agent-tui wait --sequence "$((SEQ + 1))"
agent-tui snapshot
```

Sequence-based waits are racey-free: even if the output arrives
*before* the `wait` is issued, the daemon remembers it and returns
immediately when `wait --sequence` matches.

## Event types
<!-- tested-by: navigation -->

The recorder writes asciicast-v3 with extension events:

| Type | When |
|---|---|
| `o` | Output bytes from the PTY |
| `i` | Input bytes to the PTY |
| `r` | Resize event (cols, rows) |
| `m` | Command marker — every CLI op records one with `{kind, ok, error_code}` |
| `s` | Checkpoint — pushed every 1000 PTY mutations by the per-pane task |
| `g` | Generation tick — incremented when a snapshot is taken |

`wait --sequence` walks all event kinds; `wait --text` only looks at
the rendered screen (which is computed from `o` events).

## Coalescing: hash stability across cosmetic churn
<!-- tested-by: bwrap_fzf_opens_with_candidate_list -->

The hash returned in snapshots **excludes** cells that changed only
in:

- Cursor position (when the cursor isn't on a printable cell)
- SGR-only changes (color/style without content change)
- Blink-state cycles (the engine doesn't bake animation into hashes)

So `wait --hash <prev>` doesn't false-positive on a vim cursor blink
or a spinner that's just toggling between two glyphs. Programs that
*do* update content (line redraws, status bars) advance the hash.

If you need a stricter "any byte changed" wait, use `--sequence`
instead.

## Per-pane queueing
<!-- tested-by: untested (per-pane mpsc queue deferred to P3; today's Mutex serialization is correct but not load-tested) -->

Future (P3): each pane has its own command queue. Today, the daemon
serializes `wait`/`press`/`type` per pane via a Mutex — same
outcome, simpler. The implication: `press; snapshot` can rely on the
implicit barrier that `press` returns only after the engine
ingested the input.

This is called the **press-then-quiesce barrier** in the RFC.

## Timeouts
<!-- tested-by: untested (default 25s timeout works in practice; we lack a scenario that asserts the precise timeout path) -->

Every `wait` has a default 25-second timeout (configurable via
`--max <ms>`). A timed-out wait exits non-zero with the snapshot
state at exit, so the agent can diagnose "I expected X but the screen
showed Y."

The global `--timeout <MS>` flag applies to all commands, not just
wait. `--max` on `wait` is the per-call override.

## Tested wait patterns
<!-- tested-by: bwrap_fzf_select_outputs_selection_to_stdout -->

The fzf scenarios exercise the full pattern:

```bash {test=full-wait-pattern}
agent-tui spawn -- bash -c "echo -e 'a\nb\nc' | fzf"
agent-tui wait --text "3/3"                # program is ready
agent-tui type "b"
agent-tui wait --text "1/3"                # filter applied
agent-tui press "<cr>"                     # select
agent-tui wait --idle 200                  # let stdout flush
agent-tui snapshot                         # capture final state
```
