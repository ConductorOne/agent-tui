---
name: intent
description: Verbs grouped by what you're trying to do (ask/edit/watch/run), not by capability surface
allowed-tools: Bash(agent-tui:*)
---

# Intent-shaped verbs

Pick the verb by what you're trying to accomplish. Each is sugar
over the capability primitives (`spawn` / `wait` / `snapshot` / …);
they exist so first contact with the tool doesn't require reading
the full surface.

## Read [core](../core/SKILL.md) first
<!-- tested-by: navigation -->

This skill assumes the core verbs exist. It only documents the
intent-shaped wrappers.

## Ask an AI CLI a question
<!-- tested-by: bwrap_vim_opens_file_and_shows_content -->

```bash {test=ask-claude}
agent-tui ask claude "what is 40+2"
# → 42

agent-tui ask opencode "refactor this function"
agent-tui ask pi "summarize the README"
```

`ask` knows the per-CLI flags (claude → `-p`; opencode → `run --pure
--title fixed --dangerously-skip-permissions` inside `bash -c "cat
| ..."`; etc.). One verb, no incantation.

**Limitations.** Slow runs (>25s) hit the client's one-shot read
timeout. For longer asks, use `agent-tui run --max 120000` directly
or wait until that timeout becomes configurable.

## Run any CLI
<!-- tested-by: navigation -->

For non-AI CLIs without a recipe — or with custom flags — use
`run` directly:

```bash
agent-tui run --stdin "your input" -- some-cli --flags
```

See [core](../core/SKILL.md) for the full `run` surface.

## Edit a file
<!-- tested-by: untested (covered by vim integration tests but not via the `edit` verb directly) -->

```bash {test=edit}
agent-tui edit /work/notes.md             # opens $EDITOR (default vim)
EDITOR=helix agent-tui edit /work/x.rs    # override editor
agent-tui edit /work/x.rs --editor nano   # explicit override
```

Blocks until the editor exits, then prints the file's content.
Useful as a one-shot "edit and capture" without writing a wrapper.

## Watch a long-running command
<!-- tested-by: untested (covered by tail --follow integration; needs a dedicated test for the `watch` verb) -->

```bash {test=watch}
agent-tui watch -- tail -f /var/log/syslog
agent-tui watch -- bash -c "for i in {1..10}; do echo step \$i; sleep 1; done"
```

Spawns the child + streams its output to stdout via `tail --follow`.
Exits when the child exits.

## When to drop down to primitives
<!-- tested-by: navigation -->

- Multi-step interactive driving (vim edit + save + diff) → use
  `spawn` + `press` + `wait`.
- Observing mid-flight state (snapshot during a long agent run) →
  `spawn` + `tail --follow` in one terminal, `snapshot` calls in
  another.
- Custom AI CLI without a recipe → `run --stdin` directly; consider
  adding a recipe upstream.
