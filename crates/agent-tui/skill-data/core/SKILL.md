---
name: core
description: agent-tui usage — spawn, snapshot, wait, interact with PTY apps via @eN refs
allowed-tools: Bash(agent-tui:*)
---

# agent-tui core

`agent-tui` drives terminal / TUI apps (vim, shell, htop, claude-code,
opencode, …) from an AI agent. A per-session daemon owns the PTY and
serves an outline-based snapshot with `@eN` element refs so agents can
read screens in a few hundred tokens instead of parsing terminal bytes.

This page is the canonical entry point. Specialized skills cover
specific PTY apps:

```bash
agent-tui skills list                # all available skills
agent-tui skills get core --full     # this page + references + templates
```

## Two modes
<!-- tested-by: navigation -->

There are two distinct ways to drive a child:

**Mode A — subprocess as data** (`agent-tui run`). The child has a
non-interactive mode (`claude -p`, `gh api`, `gpg`, `jq`, …). You
want stdin in, stdout/exit-code out. One verb does it all.

**Mode B — interactive driving** (`spawn` + `snapshot` + `press` +
`wait` + …). The child has no headless mode (vim, htop, lazygit) or
you specifically want to observe its TUI. The capability primitives
let you drive it step by step.

## Mode A: just run a CLI
<!-- tested-by: bwrap_vim_opens_file_and_shows_content -->

```bash {test=run-headline}
# headless CLI: agent feeds prompt via stdin, reads answer from stdout
agent-tui run --stdin "what is 40+2" -- claude -p
# → 42

# JSON output for an agent caller
agent-tui --json run --stdin "what is 40+2" -- claude -p
# → {"argv":["claude","-p"], "exit_code":0, "stdout":"42\n", "elapsed_ms":2064}

# without stdin
agent-tui run -- gh api /user
```

`run` bundles `spawn --stdin pipe` + write-stdin + close-stdin +
`wait --exit` + `tail --strip-ansi` + cleanup. It shuts the
per-session daemon down on exit (use `--keep-daemon` to keep it for
follow-up calls).

## Mode B: the interactive core loop
<!-- tested-by: bwrap_vim_opens_file_and_shows_content -->

```bash {test=core-loop}
agent-tui spawn -- vim /fixtures/sample.txt     # 1. Spawn a PTY
agent-tui wait --text "sample.txt"              # 2. Wait for ready
agent-tui snapshot --mode outline               # 3. Read the screen
agent-tui press ":q!<cr>"                       # 4. Act
agent-tui die                                   # 5. Tear down
```

Per-app adapters emit **durable refs** keyed by what they identify
(`@vim.buffer`, `@shell.prompt`, `@tmux.pane[%2]`). These survive
across snapshots — same ref, same logical node. You do NOT need to
re-snapshot before each ref-based interaction.

The generic adapter (when no per-app adapter matches) still emits
positional `@eN` refs that are frame-local — that's the only place
the old "re-snapshot first" rule applies. Check the node's `durable`
field if you need to know which kind you got.

See `agent-tui skills get addressing` for the selector grammar and
the `press --to '<selector>'` routing model.

## Spawning a process
<!-- tested-by: bwrap_htop_renders_process_list_and_fkeys -->

```bash {test=spawn}
agent-tui spawn -- htop --no-color                  # PTY stdin (interactive default)
agent-tui spawn --stdin pipe -- claude -p           # pipe stdin (headless CLI)
agent-tui spawn --stdin closed -- env               # /dev/null stdin
```

`--stdin pipe` gives the child a pipe instead of a TTY for stdin —
required by CLIs that do `isatty(0)` and refuse a TTY stdin
(`claude -p`, `gh api`, `gpg`). The daemon keeps the write end so
later `stdin` / `close-stdin` calls can feed it.

`--stdin closed` ties stdin to `/dev/null`. Programs that try to
read see EOF immediately. Right for `--print`-style CLIs that take
the prompt as an argument.

Spawn returns a JSON envelope with the pane id (e.g. `p1`). The
session has one pane by default; multi-pane workflows pass `--pane`
explicitly.

## Writing to a child's stdin pipe
<!-- tested-by: untested (stdin/close-stdin work for all probed CLIs; integration test for these specific verbs is the agent-tui run sugar's own tests) -->

For panes spawned with `--stdin pipe`:

```bash {test=stdin-verbs}
agent-tui stdin --text "hello world"            # write literal bytes
agent-tui stdin --bytes-hex "68690a"            # or hex
agent-tui close-stdin                           # EOF the child's stdin
```

For PTY-stdin panes, use `type` / `press` / `send-ansi` instead —
those write through the master PTY (which IS stdin for PTY mode).

## Reading raw output bytes
<!-- tested-by: untested (tail's core path is exercised by every run --stdin test; needs a dedicated integration scenario) -->

```bash {test=tail}
agent-tui tail                                  # all bytes the child has written
agent-tui tail --strip-ansi                     # plain UTF-8 text (no escape sequences)
agent-tui tail --since 1024                     # bytes from offset 1024 onward
agent-tui tail --follow --strip-ansi            # stream new bytes live to stdout
agent-tui --json tail --follow                  # NDJSON envelope per chunk + final {type:"eof"}
```

`tail` returns `next_since` so a follow-up call (`tail --since
<next_since>`) returns only NEW bytes. For live observation of a
slow child, use `--follow`: the daemon streams chunks as bytes
arrive and terminates with a `{type:"eof", exit_code:N}` envelope
when the child exits.

## Editing a file and saving
<!-- tested-by: bwrap_vim_edit_save_round_trip -->

```bash {test=edit-save}
agent-tui spawn -- vim /work/notes.md
agent-tui wait --text "notes.md"
agent-tui press "i hello<esc>"                  # enter insert, type, leave insert
agent-tui wait --text "\[\+\]"                  # vim's modified-marker
agent-tui press ":w<cr>"
agent-tui wait --text "written"
agent-tui snapshot
agent-tui press ":q<cr>"
```

`press` accepts vim-style key notation: `<cr>` newline, `<esc>`,
`<f5>`, `<c-c>`, etc. Plain text inside is typed literally.

## Typing literal text
<!-- tested-by: bwrap_fzf_typed_filter_narrows_candidates -->

```bash {test=type}
agent-tui spawn -- bash -c "echo -e 'apple\nbanana\ncherry' | fzf"
agent-tui wait --text "3/3"
agent-tui type "ban"                            # types b-a-n literally
agent-tui wait --text "1/3"
agent-tui snapshot
agent-tui press "<c-c>"                         # cancel
```

`type` is for literal text (no key-notation parsing). `press` is for
key combinations. `send-ansi` is for raw escape sequences when you
need to emit bytes the PTY isn't parsing through readline.

## Sending raw ANSI / escape sequences
<!-- tested-by: bwrap_less_search_finds_anchor -->

```bash {test=send-ansi}
agent-tui spawn -- less /work/log.txt
agent-tui wait --text "log.txt"                 # less shows the FILENAME, not a ":" prompt
agent-tui send-ansi "/needle\r"                 # send literal /, needle, CR
agent-tui wait --text "needle"
agent-tui snapshot
agent-tui press "q"
```

Use `send-ansi` for application-level escape codes (slash-search in
less, OSC sequences, mode toggles). Anything you'd type at the
terminal verbatim.

## Waiting for screen state
<!-- tested-by: bwrap_fzf_opens_with_candidate_list -->

```bash {test=wait-text}
agent-tui spawn -- bash -c "echo -e 'a\nb\nc' | fzf"
agent-tui wait --text "3/3"                     # regex over the screen
agent-tui wait --idle 150                       # 150ms of no changes
agent-tui snapshot
```

`wait` has five shapes. Pick the most specific one:

| Form | Use it when |
|---|---|
| `wait --ref '<selector>' [--gone]` | A structured node is the ground truth (preferred — see [addressing](../addressing/SKILL.md)) |
| `wait --text "regex"` | No adapter outline available; matching rendered cells |
| `wait --hash <hash>` | Loop continuation — "wait until the screen differs from this hash" |
| `wait --sequence <n>` | Event-stream sync — "wait until event >= n" |
| `wait --idle <ms>` | Last-resort settling time (read [references/wait-and-events.md](references/wait-and-events.md) first) |

See [references/wait-and-events.md](references/wait-and-events.md) for
the sequence-number model and why `--idle` is a code smell when used
alone.

## Reading the screen
<!-- tested-by: bwrap_vim_search_finds_target -->

```bash {test=snapshot-modes}
agent-tui snapshot                              # default = outline
agent-tui snapshot --mode outline               # compact, ref-bearing tree
agent-tui snapshot --mode text                  # visible cells as plain string
agent-tui snapshot --mode cells                 # RLE cell grid (engine-correctness)
agent-tui snapshot --mode adapter               # adapter-specific tree
agent-tui snapshot --mode hybrid                # all four (verbose)
```

`outline` is the default and the right answer for most TUI apps. Use
`text` when the pane is just unstructured output and you want "what
does the screen say" as a plain string — much easier than parsing
the RLE/base64 cells.

Reach for `cells` when you need exact cell positions / wide chars /
SGR colors (engine-correctness tests). `adapter` when the per-program
adapter has extra structure (vim's mode + filename, shell's
prompt-vs-running).

Snapshot output includes `@eN` refs you can use in subsequent commands
(planned in P2; v0.1 returns refs as part of the outline tree). See
[references/snapshot-refs.md](references/snapshot-refs.md) for the
deep dive on ref lifetime and the four modes.

## Resizing the pane
<!-- tested-by: bwrap_less_jump_to_end_shows_end_marker -->

```bash {test=resize}
agent-tui resize 120 40                         # 120 cols × 40 rows
agent-tui snapshot
```

PTY-aware apps repaint on receipt of `SIGWINCH`. Some redraw
immediately; others wait for input. Pair with `wait --idle` if you
need the new layout before snapshotting.

## Sending signals
<!-- tested-by: bwrap_htop_setup_screen_opens_and_closes -->

```bash {test=signal}
agent-tui spawn -- htop --no-color
agent-tui wait --text "F10"
agent-tui press "q"                             # graceful quit (preferred)
# If the app is wedged:
agent-tui signal TERM                           # SIGTERM
agent-tui signal KILL                           # SIGKILL — last resort
```

`die` is the cleanup-and-exit shortcut that any test should call at
the end of a scenario.

## Focusing across multiple panes
<!-- tested-by: mcp_drives_vim_bwrap::mcp_drives_vim_through_bwrap_end_to_end -->

```bash {test=focus}
agent-tui pane focus p1                         # explicit focus
agent-tui pane list                             # see what's open
```

Focus has three states: `Auto` (most-recently-spawned wins), `Focused`
(this pane explicitly), `Held` (focused-pane death promotes to Held;
explicit refocus required).

## Recording a session
<!-- tested-by: bwrap_vimtutor_walkthrough_lessons_1_1_to_1_3 -->

The daemon writes an asciicast-v3 trace to
`$XDG_STATE_HOME/agent-tui/<session>/<pane>.cast` for every session.
Replay with `asciinema play <file>`.

```bash {test=recorder}
# The recorder is on by default. To inspect:
ls $XDG_STATE_HOME/agent-tui/default/
asciinema play $XDG_STATE_HOME/agent-tui/default/p1.cast
```

See [references/recording.md](references/recording.md) for rotation,
retention, and the v3-extension event types (markers, checkpoints).

## Diagnosing install issues
<!-- tested-by: agent_tui_doctor_runs_inside_container -->

```bash {test=doctor}
agent-tui doctor                                # full diagnosis
agent-tui doctor --json                         # structured output
```

`doctor` checks: bwrap availability (sandboxed scenarios),
PTY/terminfo basics, recorder write path, socket-dir permissions,
agent-tui binary version vs. embedded skill version. Run before
filing a bug.

## Cleaning up
<!-- tested-by: bwrap_vim_opens_file_and_shows_content -->

```bash {test=cleanup}
agent-tui die                                   # this session
agent-tui daemon shutdown                       # all sessions on this socket
```

The daemon also exits on PR_SET_PDEATHSIG when the CLI process tree
dies, on idle timeout (default 5min), or when the monitored parent
PID exits. Integration tests rely on the parent-PID monitor for
zombie-free teardown.

## When to load another skill
<!-- tested-by: navigation -->

- **Selectors, refs, and routing** (DOM-lite addressing model — read
  this whenever you're using `--ref`, `--select`, or `--to`):
  `agent-tui skills get addressing`
- **Shell sessions** (bash/zsh with OSC 133): `agent-tui skills get shell`
- **vim / nvim editing**: `agent-tui skills get vim`
- **Driving an AI CLI** (opencode, pi, codex from outside): `agent-tui skills get ai-cli`

## Full reference
<!-- tested-by: navigation -->

```bash
agent-tui skills get core --full
```

Pulls in:

- `references/commands.md` — every subcommand, flag, alias (autogenerated from `--help`)
- `references/snapshot-refs.md` — snapshot modes + `@eN` lifetime
- `references/wait-and-events.md` — `wait --text/--hash/--idle/--sequence` semantics
