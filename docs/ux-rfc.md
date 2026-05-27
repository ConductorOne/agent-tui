---
type: rfc
title: "agent-tui — UX & Surface-Area Improvements for the Long Tail"
status: draft
author: Paul Querna (with claude-opus)
created: 2026-05-27
harness: claude
---

# RFC: agent-tui — UX & Surface-Area Improvements for the Long Tail

- **Status:** Draft v0
- **Companion to:** `docs/RFC.md` (architecture v3), `docs/skills-rfc.md` (docs system)
- **Trigger:** Reflection from writing `scripts/ask-claude.sh` plus the
  pi/opencode fake-inference integration tests plus the skills system.
  Three independent rounds of "what felt awkward" converged on the same
  small set of structural gaps.

## 0. TL;DR

agent-tui's primitives — `spawn` / `snapshot` / `press` / `type` /
`send-ansi` / `wait` — are the right *capability* layer. But the
*intent* layer is missing. Agents driving CLIs hit three recurring
papercuts:

1. **"Just run this thing and tell me what it said"** is the dominant
   use case for headless-capable AI CLIs (`claude -p`, `gh api`,
   `gpg`, anything with isatty-detection). Today this requires a shell
   pipeline (`cat | foo | tee file; echo MARKER`) inside `spawn`.
2. **Output extraction** is hostile for ad-hoc parsing: cells are
   RLE+base64, outline is adapter-dependent, the raw bytes the child
   wrote aren't exposed at all.
3. **The long tail of TUI apps** (tmux, helix, k9s, btop, ranger, …)
   each need an adapter to feel "first-class" but writing Rust per app
   doesn't scale.

This RFC proposes seven long-term surfaces and one structural shift —
**adapter manifests** — that together let agent-tui be the natural
default across both "subprocess as data" and the long tail of
interactive apps, without two parallel codebases.

**Scope note.** Auth/secrets and the MCP intent layer are explicitly
out of scope here; they fill in cleanly once the core model below is
solid. See §10 for what's parked and why.

## 1. Reflection: where the friction actually lives

### What worked

- The **per-session daemon** model — lazily spawned, multiplexed over a
  unix socket — is correct. Zombie management with PR_SET_PDEATHSIG +
  idle timeout + parent-PID monitor is solid.
- **Sequence numbers + hashes** for `wait` are racey-free and the
  right primitives for "wait until something changed."
- **Adapter trait + auto-detection** is the right factoring; vim's
  structured outline is genuinely more useful than cell-grep.
- **Skills bundled in the binary** (cycle 1-5 of skills-rfc.md) makes
  the agent-facing docs version-locked to behavior.
- **Recorder + asciicast** captures everything; integration tests can
  do post-mortem analysis from the cast.
- The **bwrap-based integration harness** plus the in-process
  fake-inference server is a strong testing pattern that already
  caught several real bugs.

### Where it shows seams

| Surface | Symptom | Root cause |
|---|---|---|
| Driving a `--print`-style CLI | `cat \| foo \| tee file; echo DONE` | PTY-only stdin; no pipe-stdin mode |
| Reading what a child wrote | RLE+base64 cells, adapter-dependent outline | No "raw stdout bytes" surface |
| Streaming progress | wait-and-snapshot loops | No streaming output primitive |
| Multi-step driving | Spawn → wait → press → wait → snapshot → die | No composition verb |
| Driving an unknown TUI | Generic adapter is generic; no per-app structure | Adapter ecosystem is Rust-only |
| "Done" detection | Marker-echo tricks, text-matching, fragile | `wait --exit` exists but is one of seven, equally weighted |
| Auth / secrets | Each fixture ships its own pattern | P3 vault deferred; no current convention |
| Discovering what verb to use | `skills get core` lists 20+ flags | Verbs are capability-shaped, not intent-shaped |

The common thread: **agent-tui exposes the right capabilities but the
wrong defaults for the common case.** Capability-layer APIs are
correct for power users; intent-layer APIs are correct for agents.
The product needs both.

## 2. The fundamental tension

There are two distinct uses of agent-tui that look similar but want
opposite APIs:

**Mode A — Subprocess as data.** The child has a non-interactive
mode (`-p`, `--print`, batch, etc.) and the agent wants stdin/stdout
as data. Snapshots are noise; the pane is irrelevant; the answer is
*bytes the child wrote*. Examples: `claude -p`, `gh api`,
`gpg --decrypt`, `jq`, `kubectl describe`.

**Mode B — Interactive driving.** The child has no headless mode (or
the agent specifically wants to test the TUI). Snapshots, refs, and
adapter-aware structure are the whole point. Examples: vim, htop,
lazygit, claude TUI, tmux.

Today agent-tui ships Mode B primitives only and Mode A falls out as
a degenerate case (spawn with stdin-redirect tricks, then ignore the
pane). The friction we observed is almost entirely Mode A trying to
work through Mode B's API.

**The thesis of this RFC:** add a small set of Mode-A-first verbs
(`run`, `tail`, `stdin`, `close-stdin`), keep Mode B's verbs
unchanged, and let the daemon serve both with the same underlying
state machine.

## 3. Seven long-term surfaces

Ordered by impact-per-LOC, not by phase.

### 3.1 `agent-tui run` — Mode A's first verb

```bash
agent-tui run [--stdin <text>] [--stdin-file <path>] [--timeout <ms>]
              [--env K=V]... -- <argv>...
```

Spawns the child with a pipe for stdin (not a PTY), waits for exit,
returns:

```json
{
  "exit_code": 0,
  "stdout": "42\n",
  "stderr": "",
  "elapsed_ms": 2867,
  "sequence_at_exit": 142
}
```

No session bookkeeping required; the daemon spawned for this `run`
auto-deletes when the child exits. From `scripts/ask-claude.sh`'s
perspective, the entire script becomes:

```bash
agent-tui run --stdin "what is 40+2" --json -- claude -p \
  | jq -r .data.stdout
```

`run` is the "agent-browser eval" of agent-tui — the verb that admits
80% of headless-CLI use cases without exposing the daemon at all.

**Implementation note:** `run` is sugar over `spawn --stdin pipe`
+ `tail` + `wait --exit`. Each underlying primitive ships
independently; `run` is the bundle.

### 3.2 `agent-tui spawn --stdin <mode>` — pipe vs PTY stdin

Today the daemon always gives the child a PTY for all three FDs.
That's correct for interactive apps; it's a tax for everything else,
because programs that do `isatty(0)` will refuse to read from stdin
or behave differently.

```bash
agent-tui spawn --stdin pipe   -- claude -p     # stdin pipe, stdout/err PTY
agent-tui spawn --stdin tty    -- vim           # all three PTY (default)
agent-tui spawn --stdin closed -- env           # /dev/null on stdin
```

This is a one-line clap addition plus ~50 LOC in the spawn handler
to use `socketpair` or `pipe` for the stdin FD when requested.
Removes the `cat |` hack entirely.

### 3.3 `agent-tui tail` — raw bytes since sequence N

```bash
agent-tui tail [--since <seq>] [--max <bytes>] [--strip-ansi]
               [--follow] [--pane <id>]
```

Streams the bytes the child has written, since a sequence checkpoint
or the start of the session. With `--follow`, becomes a tail-f-style
stream. With `--strip-ansi`, runs through a state machine that
strips SGR/CSI sequences and emits printable text.

This is what agents *actually want* for Mode A children. The
recorder already has the data; we just need to expose it on the
wire.

### 3.4 `snapshot --mode text` — visible cells as plain text

```bash
agent-tui snapshot --mode text [--strip-ansi]
```

The visible terminal grid as a single string, rows joined with `\n`,
trailing whitespace stripped per row. No RLE, no base64, no adapter
dependency.

For the agent that wants "what does the screen say *right now*" —
this is the answer. Cells mode stays for engine-correctness tests;
text mode is the agent-facing default for Mode B.

### 3.5 `agent-tui stdin` / `close-stdin` — first-class stdin verbs

```bash
agent-tui stdin "what is 40+2\n"      # write bytes; same as type+press <cr> for cooked TTY
agent-tui close-stdin                  # close the FD cleanly
```

Today: typing into stdin works via `type` (literal text) + `press`
(key tokens). Closing stdin requires `press <c-d>` and assumes
canonical mode. Both fail for `--stdin pipe` children.

Two clean verbs (`stdin <bytes>`, `close-stdin`) work regardless of
the child's terminal mode. The TTY-only `type`/`press` stay for
interactive apps that care about key events.

### 3.6 Structured lifecycle events

The daemon's event stream today: output bytes, input bytes, resize,
adapter promotion, marker, checkpoint, generation. Add three
canonical lifecycle events:

| Event | Fires when |
|---|---|
| `child.spawned` | After fork+exec succeeds |
| `child.read_attempt` | First `read(stdin)` syscall observed (ptrace-light; Linux-only initially) |
| `child.exited` | Reaped, with exit code |

`wait --event child.read_attempt` solves the "is the child ready for
input yet?" problem currently handled by `sleep 1`.
`wait --event child.exited` is a clearer name for today's
`wait --exit` and slots naturally into the same dispatcher.

This generalizes: future event types (`child.first-prompt`,
`adapter.promoted`, `tool.invoked`) can ride the same wire without
new flag flavors.

### 3.7 Adapter manifests — adapters as data

The killer long-term move. Today an adapter is a Rust module that
implements the `Adapter` trait. That's the right design for
sophisticated cases (claude-code parses panel structure, vim parses
modelines + cmdline state), but it's overkill for "this app uses
alt-screen, has a top status bar, footer key hints, and a scrollable
main pane" — which describes 80% of TUI apps.

Propose a YAML/TOML/CUE **manifest format** that the daemon loads at
runtime:

```yaml
# adapters/lazygit.yaml
name: lazygit
detect:
  argv0: ["lazygit"]
  banner_regex: '^lazygit '
outline:
  panels:
    - name: status
      anchor: { row: 0, col_range: [0, 30] }
      role: status-bar
    - name: files
      anchor: { row_range: [1, -2], col_range: [0, 40] }
      role: list
    - name: main
      anchor: { row_range: [1, -2], col_range: [41, -1] }
      role: detail
  state:
    insert_mode_marker: "INSERT"     # if visible, classify mode=insert
adapter_aware_waits:
  - name: panel_focus_changed
    detect: "hash-of-current-focus-cell-changed"
```

The daemon's "manifest-driven adapter" runs the spec against each
engine snapshot to build the outline. No Rust code, no recompile,
ships in `~/.config/agent-tui/adapters/` or built-in under
`crates/agent-tui-adapter/manifests/`.

What this unlocks:
- **A user can add a new adapter without contributing to the repo.**
- **Per-version drift handling** — pin a manifest to lazygit v0.40+.
- **Community ecosystem** — manifests as gists, downloaded with
  `agent-tui adapter install <url>` (sha256-verified).
- **Test parity for free** — manifests are testable in isolation,
  separate from the engine.

This is the same shift agent-browser made from "Playwright actions
in code" to "find role / find text" semantic locators. The locators
are the language of intent.

**Tension to resolve:** vim's adapter needs to PARSE the modeline
("INSERT -- sample.txt" + `[+]` modifier + `1,1` cursor position),
which is harder than a region spec. Manifests need an embedded
mini-language for that. CUE or starlark are candidates. The
practical answer is probably "manifest covers 80%, Rust adapter for
the rest, with a clean fallthrough."

### 3.8 Replay-as-regression — recordings as a test corpus

Today the recorder writes asciicast-v3 with extensions. Replay is
"asciinema play this file." Make the cast a **first-class test
input**:

```bash
agent-tui replay --cast tests/regressions/lazygit-v0.40-quit.cast \
                 --expect-snapshot tests/regressions/lazygit-v0.40-quit.snap
```

The daemon re-feeds the captured byte stream into a fresh engine,
takes a snapshot, diffs against the expected snapshot. If the engine
parses bytes differently in v0.2 than v0.1, the diff catches it.

For the ecosystem: every documented use case in `skill-data/**` could
ship with a small cast. CI runs them all every release.

## 4. The "wide variety of TUI apps" question

Today's integration test list covers: vim, fzf, htop, less, lazygit,
tig, nano, claude-code, opencode, pi, shell-with-osc133. That's
about 11 apps with bespoke fixtures.

The long tail an agent might want to drive: tmux, screen, mosh,
nvim, emacs, helix, kakoune, k9s, lazydocker, btop, glances,
nvtop, ranger, nnn, lf, mc, weechat, irssi, mutt, neomutt, alpine,
ncmpcpp, cmus, joshuto, yazi, gitui, jujutsu's `jj log`, ncdu,
gotop, zenith, bandwhich, dust, …

Three approaches to covering them:

| Approach | Pros | Cons |
|---|---|---|
| Built-in Rust adapter each | Best quality | Doesn't scale; vendoring nightmare |
| Pure-generic adapter | Already works, no per-app code | Lowest common denominator; outline is structural noise |
| Manifest-driven (§3.7) | User-contributable, version-pinnable, fast to write | Limited expressiveness; can't beat code for hard cases |

The recommendation: **manifests are the default; Rust adapters are
the exception.** Treat the manifest format as the public API; treat
Rust adapters as a compile-time optimization for the apps where
matters enough.

Adoption path:
1. Ship the manifest engine + 3-5 manifests for currently-Rust adapters
   (lazygit, tig, htop, less, fzf) to prove the format works.
2. Keep the Rust adapter trait for vim, claude-code, shell (the
   complex three).
3. Open a `~/.config/agent-tui/adapters/` directory + a fetcher
   (`agent-tui adapter install <url>`).
4. CI runs ALL bundled manifests against ALL bundled casts every
   release — full matrix regression suite.

## 5. Discovery: verbs ranked by intent

The skills system today groups by adapter (core/shell/vim/ai-cli).
Add a layer above it: **verbs grouped by intent**.

```
agent-tui skills get verbs

Common intents:
  Run a CLI and get its output   → agent-tui run …
  Read a file under an editor    → agent-tui edit …    (sugar over spawn + extract)
  Browse a tree (k9s, ranger)    → agent-tui browse …  (sugar over spawn + interact)
  Tail a log                     → agent-tui watch …   (sugar over spawn + tail --follow)
  Drive an AI CLI                → agent-tui ask …     (sugar over run)
```

These are convenience commands. They lower the bar for the first
contact with the tool. None of them displace the primitives; each
expands into a known primitive sequence.

The risk: a sugar verb that *almost* fits a use case is worse than
no sugar (the agent picks it, then has to back out). Mitigation:
keep the sugar verbs small and behave-as-expected; the primitive
sequence stays one `--help` away.

## 6. The cohesive picture

If all seven surfaces land:

```bash
# Subprocess as data — the 80% case
agent-tui run --stdin "$prompt" -- claude -p

# Streaming output without snapshot loops
agent-tui spawn --stdin pipe -- gh repo list
agent-tui tail --follow --strip-ansi --pane p1

# Interactive driving with adapter awareness
agent-tui spawn -- helix /work/notes.md      # uses manifest-driven helix adapter
agent-tui press "ihello<esc>:w<cr>"
agent-tui snapshot --mode text

# Reproducible regression
agent-tui replay --cast regressions/htop-v3.3.cast \
                 --expect-snapshot regressions/htop-v3.3.snap

# Intent-shaped sugar
agent-tui ask --stdin q.txt -- claude -p
agent-tui edit /work/notes.md
agent-tui watch /var/log/syslog
```

The capability surface (spawn / wait / type / press / snapshot) is
unchanged. The intent surface above it is new and small.

## 7. Phased rollout

| Phase | Lands | Status |
|---|---|---|
| **P-UX1** | `spawn --stdin {pipe,tty,closed}`, `tail [--since --strip-ansi]`, `snapshot --mode text` | ✅ landed |
| **P-UX2** | `run` sugar verb, `stdin`/`close-stdin`, `wait --exit` returns exit_code | ✅ landed |
| **P-UX3** | Manifest-driven adapter engine + manifests for 5 existing Rust adapters; deprecate the duplicated Rust code | pending |
| **P-UX4** | `replay` + cast-driven regression CI | pending |
| **P-UX5** | `ask` / `edit` / `watch` sugar verbs; verbs-by-intent skill page; `tail --follow` | pending |

P-UX1 + P-UX2 collapsed `ask-claude.sh` from 89 lines to 18:

```bash
exec agent-tui run --stdin "${1:-what is 40+2}" -- claude -p
```

(Critical fix landed alongside: O_CLOEXEC on the stdin pipe and
F_DUPFD_CLOEXEC on the duped slave fds. Without this, the child
inherits the daemon's pipe fds and holds the write end open against
itself — `close-stdin` then doesn't EOF the read end, and `wait
--exit` hangs. The bug masqueraded as a claude-specific quirk until
fd-table inspection traced the root cause.)

P-UX3 is the systematic move toward "wide variety of TUI apps."
P-UX4 makes the corpus testable. P-UX5 is the polish that makes
day-1 contact feel finished.

## 8. Non-goals (and intentional defers)

**Non-goals:**

- **Replacing `expect`/`pexpect`.** Those are great for scripted
  human-style interaction. agent-tui targets the agent-driven case.
- **Becoming a terminal multiplexer.** tmux is excellent at what it
  does; agent-tui *consumes* tmux, doesn't try to be it.
- **A general programmable shell.** We have shells. agent-tui drives
  them.
- **Cross-platform parity from day 1.** macOS + Linux first; Windows
  follows the existing strategy (`docs/windows-strategy.md`).

**Intentionally deferred to future RFCs (parked until core is solid):**

- **Auth / secrets vault.** Per-fixture today is `OPENAI_API_KEY` in
  env vars and per-CLI config files. A first-class vault + `--auth
  <name>` surface is real value, but it composes cleanly on top of
  the core (`run --auth foo -- claude -p` is a thin wrapper). Park
  until P-UX1..P-UX5 are land-tested; revisit when the shape of
  `run`/`spawn` is settled enough that auth wiring is mechanical.
- **MCP intent surface.** Today's `mcp serve` mirrors the CLI
  verbatim (capability-shaped). A parallel intent-shaped surface
  (`run_cli`, `drive_interactive`, …) cuts round-trips for agent
  users — but its shape is determined by the verbs in §3 + §5.
  Wait until those verbs are stable, then mirror them.

The reason both are parked: shipping them now means designing
against today's wobble. Once the core verbs settle, auth + MCP land
as boring wrappers rather than load-bearing redesigns.

## 9. Risks & open questions

- **Manifest expressiveness.** Will a declarative format be enough
  for, say, helix or nvim? The fallback to Rust is fine, but if every
  serious adapter needs Rust, the manifest engine is dead weight. Need
  to land 5 working manifests before betting more on the format.
- **`run` vs `spawn` confusion.** Two verbs for "start a process"
  is a footgun. Could be solved with `spawn --oneshot` instead of a
  new verb. Tentative: ship the new verb; aliases are cheap.
- **Manifest distribution security.** `agent-tui adapter install
  <url>` downloads code-like assets. sha256-pin everything; refuse
  install over HTTP; sign manifests with a known key set.
- **Backwards-compat.** Every new flag is forever. Choose the names
  carefully: `--stdin {pipe,tty,closed}` reads better than
  `--no-tty-stdin` etc.

## 10. What this RFC is NOT

This is not a request for implementation tomorrow. It's a target
state. The phased rollout (§9) lets us land any single phase in
isolation; each subsequent phase composes on top.

The thing this RFC *does* commit to: the next time we add a
fixture-tested AI CLI or TUI app, we should evaluate whether a
manifest covers it before reaching for Rust. If yes → it ships
without code review beyond the manifest. That's the leverage we
need to keep up with the long tail.

## 11. Implementation notes (post-landing)

The OODA loop that built P-UX1 + P-UX2 surfaced three non-obvious
findings worth recording.

### 11.1 The CLOEXEC bug

`spawn --stdin pipe` initially failed only for claude — every other
CLI worked. Hours of investigation traced the cause: the daemon's
pipe fds (and the duped slave-PTY fds in the custom-spawn path) were
inherited by the child at exec time. When the daemon closed its
write-end fd, the **child** still held a copy (at some unrelated fd
number) — so the kernel saw an active writer and never signalled EOF
to the reader. `close-stdin` worked but had no effect.

Fix: `pipe2(O_CLOEXEC)` instead of `pipe()`; `F_DUPFD_CLOEXEC`
instead of `dup()`. Tests for cat, bash, node passed despite the
bug because their close-on-exec defaults happened to mask the
inheritance.

Lesson: **any new fd the daemon allocates must be CLOEXEC-by-
default.** Add this to the architecture invariants.

### 11.2 The daemon-shutdown race

P-UX2's first cut had `run` auto-call `daemon shutdown` on exit.
Logically correct (one-shot semantics), but it raced with the next
`run`: the new client connects mid-teardown and sees "daemon closed
without responding."

Pragmatic fix: don't auto-shutdown. Rely on the 5-minute idle
timeout. A lingering daemon for 5min costs ~5MB of RSS — cheaper
than the developer-experience cost of intermittent "closed without
responding" errors. `--keep-daemon` retained as a no-op for forward
compatibility.

Possible future fix: have the daemon's shutdown handler refuse new
connections immediately and finish in-flight work, so the socket
stops being advertised before the daemon actually exits. Then the
next `run` would re-spawn a fresh daemon. Deferred.

### 11.3 `--stdin` escape interpretation

`agent-tui run --stdin "hello\nworld" -- sort` initially passed
literal `\` + `n` + `\` + `world` bytes. Agents expect printf-style
escapes in stringly-typed args; this was a UX paper cut.

Fix: a small `interpret_escapes` helper that handles `\n`, `\r`,
`\t`, `\0`, `\\`, `\"`. Unknown escapes preserve the backslash so
agents passing `\latex` don't silently lose data.

## 12. Appendix: the script that triggered this

`scripts/ask-claude.sh` today is ~80 lines:

```bash
#!/usr/bin/env bash
set -euo pipefail
PROMPT="${1:-what is 40+2}"
SESSION="ask-claude-$$"
ANSWER_FILE="$(mktemp -t claude-answer.XXXXXX)"
DONE_MARKER="__AGENT_TUI_DONE_$$__"
# … locate binary, set up trap …
"$AT" --session "$SESSION" spawn -- \
    bash -c "cat | claude -p | tee $(printf '%q' "$ANSWER_FILE"); printf '\n%s\n' '$DONE_MARKER'"
sleep 1
"$AT" --session "$SESSION" type "$PROMPT"
"$AT" --session "$SESSION" press "<cr>"
"$AT" --session "$SESSION" press "<c-d>"
"$AT" --session "$SESSION" wait --text "$DONE_MARKER" --max 60000
cat "$ANSWER_FILE"
```

After this RFC's P-UX1 + P-UX2 land:

```bash
agent-tui run --stdin "$1" --json -- claude -p | jq -r .data.stdout
```

The 80-line script becomes a one-liner. The cleanup, the marker, the
sleep, the temp file, the `cat |` hack — all gone, because each was
working around a missing primitive that this RFC defines.
