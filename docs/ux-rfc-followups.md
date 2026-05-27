---
type: plan
title: "agent-tui — Follow-on Plan (post P-UX1 + P-UX2)"
status: draft
author: Paul Querna (with claude-opus)
created: 2026-05-27
parent_rfc: ux-rfc.md
---

# Follow-on Plan: post P-UX1 + P-UX2

P-UX1 + P-UX2 landed (`spawn --stdin`, `tail`, `snapshot --mode text`,
`stdin`/`close-stdin`, `wait --exit + exit_code`, `agent-tui run` with
escape interpretation, real `daemon shutdown`). This plan sequences
the remaining surfaces in P-UX3+ into executable phases.

Each phase below has concrete tasks, exit criteria, and kill criteria.

## 0. TL;DR — phase ordering and rationale

| # | Phase | Status | Effort | Risk |
|---|---|---|---|---|
| 1 | `tail --follow` streaming | ✅ landed | S | Low |
| 2 | Adapter manifests prototype | ✅ landed (6 manifests bundled, runtime engine works) | L | Medium-High |
| 3 | Replay-as-regression | ✅ landed (`replay --expect-snapshot`); xtask corpus-walker deferred | M | Low |
| 4 | `ask` / `edit` / `watch` sugar verbs + intent layer | ✅ landed | S-M | Low |
| 5 | xtask improvements | ✅ landed (typed CLI surface; allowlist shrank) | S | Low |
| 6 | Auth vault | parked | M | Medium |
| 7 | MCP intent surface | parked | S | Low |

Phases 1–5 are independent enough to be picked up in any order if a
single phase blocks; the listed order is the recommended path.

Each phase ends with an eval (a real script driving a real CLI) that
either proves the value-add or surfaces the next friction. **The
eval is non-negotiable.** It's how we know "deeply satisfied" vs.
"shipped code."

## 1. Phase 1 — `tail --follow` streaming

### 1.1 Goal

Let an agent observe progress from a slow child without polling.
Today `agent-tui run -- some-slow-thing` is blocking + reveals
output only at exit. For long claude tool-use runs (often 30+ s),
the calling agent is blind to mid-flight state.

### 1.2 Wire shape

Two design choices, decide before building:

**A: NDJSON event stream from a single call.** `tail --follow`
returns a `Content-Type: application/x-ndjson` stream of
`{"type":"bytes","data":"...","since":N}` lines, terminated by
`{"type":"eof"}`. Client reads line-by-line.

**B: Long-poll with cursor.** `tail --since N --wait-for-bytes
--max <ms>` blocks until new bytes arrive or timeout. Client loops.

Recommendation: **A** (NDJSON). Simpler agent ergonomics, fewer
round-trips. B is a fallback if streaming through the daemon's
unix-socket framing turns out to be hard.

### 1.3 Concrete tasks

1. **Wire protocol:** add `Tail { follow: bool, ... }` flag. When
   `follow: true`, the daemon's framing switches to NDJSON.
2. **Daemon-side:** subscribe to the engine's mutation broadcast +
   per-mutation, emit the bytes that landed since the prior emit.
   Terminate on child exit. Honor `--max` for an overall timeout.
3. **Client-side:** new `agent-tui tail --follow [--strip-ansi]`
   that prints bytes (or text) to stdout as they arrive, exits 0
   when the stream ends cleanly.
4. **Smoke test:** an integration scenario that spawns a `for i in
   1 2 3; do echo step $i; sleep 0.5; done` and verifies bytes
   arrive in three distinct reads, not one.

### 1.4 Exit criteria

```bash
# Driving a 5-second progress producer:
agent-tui tail --follow --pane p1 &
agent-tui spawn -- bash -c "for i in $(seq 5); do echo step $i; sleep 1; done"
# stdout shows "step 1" ~1s after spawn, not 5s after.
```

Plus: NDJSON event stream parses cleanly with `jq` — no malformed
lines, no missing final `{"type":"eof"}`.

### 1.5 Kill criteria

If the streaming framing turns out to require a major rewrite of
the daemon's response handler (it's currently one-shot), pause and
ship option B (long-poll) instead. A is preferred but B is the
safety net.

### 1.6 Eval

Rewrite a single scenario from the OpenCode integration suite to
use `tail --follow` instead of `wait --text` polling. Compare LOC
and elapsed-time. Document the delta in the RFC.

## 2. Phase 2 — Adapter manifests prototype

### 2.1 Goal

Cover the long tail of TUI apps without writing a Rust adapter per
app. A manifest is a declarative spec the daemon loads at runtime to
build the outline from an engine snapshot.

### 2.2 Manifest format

Pick one. All three are reasonable:

| Format | Pros | Cons |
|---|---|---|
| YAML | Familiar, widely-supported | Whitespace fragility, indentation hell |
| TOML | Simpler grammar | Less ergonomic for nested structure |
| CUE | Strong typing, schema-as-data | Niche, harder to author by hand |

Recommendation: **TOML**. The structure is shallow enough that
TOML's flat tables work; serde-toml ships in the ecosystem.

### 2.3 Concrete tasks (in order)

1. **Schema spike** (1-2 days):
   - Write 5 manifests on paper (no engine yet) for: lazygit, tig,
     htop, less, fzf. These already have Rust adapters or are
     well-understood — manifests should reproduce their behavior.
   - Compare. Iterate on the schema until all 5 fit comfortably.
   - Document the schema in `docs/adapter-manifests.md`.

2. **Engine** (3-5 days):
   - `crates/agent-tui-adapter/src/manifest.rs`: load+parse TOML,
     evaluate against an `EngineSnapshot`.
   - Run-time predicates: `row`, `row_range`, `col`, `col_range`,
     `regex`, `cell_at`. Boolean-combined.
   - Outline node generation from predicate matches.

3. **Wiring** (1 day):
   - `AdapterRegistry::register_manifest(path)` — loads + adds.
   - Daemon's startup walks `~/.config/agent-tui/adapters/*.toml`.
   - Built-in fallback: bundle the 5 starter manifests via
     `include_str!`.

4. **Integration tests** (2-3 days):
   - Port one existing bwrap adapter test per manifest. Assert
     identical-or-better outlines vs the Rust adapter version.
   - Add a "regression corpus" cast file per app (Phase 3 will
     extend this).

5. **Distribution** (1-2 days):
   - `agent-tui adapter install <url> --sha256 <hex>` —
     sha256-verified download into `~/.config/agent-tui/adapters/`.
   - `agent-tui adapter list` — show loaded adapters + sources.
   - `agent-tui adapter validate <path>` — schema check + dry-run
     against a saved cast.

### 2.4 Exit criteria

- 5 manifest-driven adapters land alongside their Rust equivalents.
- Per-app integration tests pass against EITHER backend (set via
  env var for switching).
- New apps (the long tail): documented a 30-minute add-an-adapter
  recipe in `docs/adapter-manifests.md` and verified by adding ONE
  new app via manifest only (suggest: `gitui` or `btop`).

### 2.5 Kill criteria

If the 5 starter manifests need >50% Rust-adapter-equivalent code
in pre-/post-processing hooks, the manifest format isn't expressive
enough. Pause; consider Starlark (programmable) instead of TOML
(declarative). Or accept that the format covers 80% and Rust
adapters cover the last 20% (which is fine — that was the
fallback plan in the RFC §3.7).

### 2.6 Eval

A community contributor with NO Rust background adds an adapter
for an app they care about, purely by editing a TOML file. If they
hit a blocker that requires Rust knowledge, the format failed —
fix the schema and re-eval.

## 3. Phase 3 — Replay-as-regression

### 3.1 Goal

Make the recorder's `.cast` files first-class test inputs. "Did the
engine parse this byte stream the same way last release?" becomes
an automatic check.

### 3.2 Concrete tasks

1. **`agent-tui replay <cast> [--expect-snapshot <path>]`** —
   re-feed the captured bytes into a fresh engine, take a snapshot,
   diff against the expected snapshot. Exit 0 on match, 1 on
   mismatch (with a structured diff to stderr).

2. **Cast capture in skill-data** — every documented
   `bash {test=…}` block in `skill-data/**` runs in CI; the
   resulting cast is committed to `crates/agent-tui/skill-data/
   casts/<test-id>.cast` plus an `.expect.json` snapshot.

3. **`cargo xtask replay-corpus`** — walks the cast corpus, runs
   every replay, reports mismatches. Hooks into CI.

4. **Per-PR snapshot diff** — if a PR changes engine code, the
   replay-corpus is re-run; CI surfaces any snapshot diff so the
   author can verify it's intentional.

### 3.3 Exit criteria

- The 5 manifest adapters from Phase 2 each have 3+ casts in the
  corpus.
- Every Pi/OpenCode integration test contributes its cast to the
  corpus.
- A deliberate engine change (e.g. a different SGR handling) makes
  CI fail with a clear diff.

### 3.4 Kill criteria

If snapshot diffs are too noisy to be useful (every refactor
shifts a cell), invest in canonicalization (drop SGR-only changes,
collapse cursor positions) before continuing. If after
canonicalization they're STILL too noisy, the engine isn't
deterministic enough — that's an engine bug, not a replay bug.

### 3.5 Eval

Land a deliberate, small engine change (e.g. handling of an
obscure CSI sequence). Verify the replay corpus catches it. If
nothing catches it, the corpus is too thin; expand it.

## 4. Phase 4 — Sugar verbs + intent layer

### 4.1 Goal

Lower the bar for first contact. The capability verbs (`spawn` /
`press` / `wait` / `snapshot`) are right for power users; the
intent-shaped verbs are right for "I just want to do X."

### 4.2 Verbs to ship

| Verb | What | Wraps |
|---|---|---|
| `ask` | Drive an AI CLI with a prompt | `run --stdin <prompt>` + per-CLI defaults |
| `edit` | Open a file in an editor, return after save | `spawn vim` + `wait --exit` + `tail` |
| `watch` | Tail a log/command output | `spawn` + `tail --follow` |
| `browse` | Drive a tree-explorer (ranger, k9s, lazydocker) | `spawn` + `press`-driven script |

`ask` is the highest-impact. Detect the CLI from argv[0] and apply
its known flags:

```bash
agent-tui ask -- claude "what is 40+2"
# expands to: agent-tui run --stdin "what is 40+2" -- claude -p

agent-tui ask -- opencode "refactor this"
# expands to: agent-tui run --stdin "refactor this" -- bash -c \
#   "opencode run --pure --title fixed --dangerously-skip-permissions"
```

The per-CLI knowledge lives in a small TOML registry — same
distribution pattern as adapter manifests.

### 4.3 Concrete tasks

1. **`ask` verb implementation** (1 day):
   - Detect CLI from argv[0] basename.
   - Apply known flag defaults from
     `~/.config/agent-tui/ask-recipes.toml` (built-in fallback for
     claude, opencode, pi, codex).
   - Allow user override: `agent-tui ask --no-recipe -- claude
     -p "..."` falls through to direct argv.

2. **`edit` verb** (1 day):
   - `agent-tui edit <file>` defaults to `$EDITOR` or `vim`.
   - Wait for the editor to exit; return the file's content.
   - Optional `--diff <reference>` returns a diff against the
     reference rather than the new content.

3. **`watch` verb** (depends on phase 1):
   - `agent-tui watch -- tail -f /var/log/syslog` ≡
     `agent-tui spawn -- tail -f ...; agent-tui tail --follow`.

4. **Intent skill page**:
   - `skill-data/intent/SKILL.md` — verbs grouped by what the user
     is trying to do, not what the CLI surface looks like.

### 4.4 Exit criteria

- `agent-tui ask -- claude "..."` works without flag incantation.
- `agent-tui edit notes.md` opens vim, returns the saved content
  on exit.
- New skill page accessible via `agent-tui skills get intent`.

### 4.5 Kill criteria

If `ask`'s per-CLI recipes need >5 lines of TOML per CLI, the
default flags surface is too wide. Either we accept that some CLIs
need a recipe-file edit, or we drop `ask` in favor of just
documenting the `run --stdin <text> -- <argv>` pattern with known
incantations.

### 4.6 Eval

A teammate who hasn't read the RFC asks "how do I run an AI CLI
through this?" — they should land at `agent-tui ask` within 30
seconds, not by reading commands.md.

## 5. Phase 5 — xtask + skills hygiene

### 5.1 Goal

Close the drift gaps surfaced during P-UX1/P-UX2:

- `cli-coverage`'s parser misses `--bool-flag`s with no value.
- `cli-coverage`'s parser misclassifies value-enum strings (`pipe`,
  `closed`) as subcommands.
- The allowlist accumulated post-P-UX2 entries because skills
  weren't updated atomically.
- No CI workflow exists yet — we're relying on local `cargo
  xtask`.

### 5.2 Concrete tasks

1. **Fix `cli-coverage` parser**:
   - Use clap's `Command` API directly via a dep on the
     `agent-tui` crate (or a thin re-export crate to avoid
     circular deps). Walking the typed clap surface beats parsing
     `--help` text.
   - Distinguish subcommands from value-enum variants explicitly.

2. **CI workflow** (`.github/workflows/coverage.yml`):
   - Run `cargo xtask docs-coverage` + `cargo xtask cli-coverage`
     on every PR.
   - Cache target/ between runs.
   - Surface failures as PR comments.

3. **Replay-corpus runner** (depends on Phase 3):
   - `cargo xtask replay-corpus` runs in CI; failures block merge.

4. **`xtask new-skill <name>` scaffolder**:
   - Creates `skill-data/<name>/{SKILL.md,_description.txt}` with
     the right frontmatter + a placeholder section.
   - Reduces the "new skill" friction from "remember the
     pattern" to "run a command."

### 5.3 Exit criteria

- `.undocumented-allowlist.txt` is empty except for explicitly-
  exempt items (the operator-facing daemon/doctor flags).
- CI passes on a fresh branch with no manual intervention.
- A new skill takes ~2 minutes to scaffold and write.

### 5.4 Kill criteria

If the typed clap surface in `cli-coverage` requires cyclic deps
or hits build-time issues, fall back to a more careful help-text
parser. The goal is signal-without-allowlist; either path
delivers.

## 6. Phase 6 — Auth vault (parked, revisit after phase 4)

Spec is in RFC §8 ("Intentionally deferred"). Revisit AFTER phases
1–4 land so the verb surface is stable. The user-facing shape will
be:

```bash
agent-tui auth save my-anthropic --env ANTHROPIC_API_KEY -
agent-tui ask --auth my-anthropic -- claude "..."
agent-tui run --auth my-openai -- gpt-4 -p
```

The vault is mlock + kernel-keyring backed (RFC.md P3 in
architecture). Secrets materialize at exec time, get torn down at
child exit, never enter the cast or artifacts.

**Pre-work needed before this phase:**
- Phase 4 must land so `--auth <name>` has stable verbs to hang on.
- Define what "auth" means per-CLI (env vars, config files, OAuth
  tokens at certain paths). Sketch a recipe format similar to
  Phase 4's `ask-recipes.toml`.

## 7. Phase 7 — MCP intent surface (parked, after phase 4)

Spec is in RFC §8. Once Phase 4's verbs are stable, add an MCP
tool surface that mirrors them:

```
tools/list returns:
  - run_cli(argv, stdin?, timeout?)         → stdout/stderr/exit
  - drive_interactive(argv, steps[])        → final snapshot
  - read_output(pane, since?, format?)      → bytes/text
  - ask(provider, prompt, options?)         → answer/exit
  - edit(path)                              → content_after_save
```

Each maps 1:1 to a CLI verb. Existing capability-shaped MCP tools
stay; this is an additional surface.

**Pre-work:** Phase 4's verbs are stable; the existing `mcp serve`
test scenarios still pass.

## 8. Cross-cutting

Run these in parallel with phases 1–5; they don't block on any
specific phase.

### 8.1 Distribution

- `cargo install agent-tui` works (today: in-tree only).
- `npm i -g agent-tui` ships the prebuilt binary.
- `brew install agent-tui` via a custom tap.

Pre-req: tags + releases on GitHub; CI builds for darwin-x64,
darwin-arm64, linux-x64, linux-arm64, linux-musl-x64.

### 8.2 Windows

`docs/windows-strategy.md` covers cycle W2 (signal mapping + `.exe`
strip). Land after phase 4 — fewer surface changes happening then.

### 8.3 Documentation

- Update `docs/RFC.md` to mention the new verbs in §5.1.
- Add `docs/agent-tui-vs-expect.md` for the "why not just use
  expect?" question.
- Land `docs/adapter-manifests.md` when Phase 2 ships.

## 9. Measurement — what "done" feels like

Per phase, measure:

1. **LOC delta** in the eval script (smaller = better).
2. **First-contact time** — fresh teammate uses the verb within
   30 seconds of reading the skill page.
3. **Friction count** — number of `# workaround:` style comments
   in eval scripts. Should approach 0.
4. **Coverage delta** — orphan test count in docs-coverage should
   shrink, not grow.
5. **Eval-script run-time** — agent-tui isn't a perf-critical
   tool, but a `run` should stay under 200ms for trivial commands
   (today: ~85ms for `bash echo`).

## 10. Sequencing summary

```
P-UX1 + P-UX2 ────────────► (landed)
                              │
                              ├── Phase 1 (tail --follow) ──┐
                              │                              │
                              ├── Phase 5 (xtask hygiene) ──┤
                              │                              │
                              ├── Phase 2 (manifests) ──────┼── Phase 3 (replay)
                              │                              │
                              └── Phase 4 (sugar verbs) ────┘
                                            │
                                            ├── Phase 6 (auth)
                                            │
                                            └── Phase 7 (MCP)
```

Phases 1 and 5 are independent and small — land them in parallel
to clear technical debt while Phase 2 (the big bet) gestates.

## 11. Initial work item — pick one and start

Highest leverage from a standing start:

**Start with Phase 1 (tail --follow).** Smallest scope, validates
the streaming-response wire pattern we'll need for Phase 2's
adapter-manifest live-reload anyway. If it lands cleanly in a
week, the team's calibrated; if it surfaces a structural issue,
better to find it now than during the bigger Phase 2.

The full P-UX1+P-UX2 implementation took ~one session. Each phase
above is estimated at 1-3 sessions of similar density. Total time
to land Phases 1–5 (skipping the parked 6 & 7): roughly 8-15
sessions of focused work.

After that — if all five phases pass their evals — the model is
"deeply satisfied" in the sense the parent RFC promised.
