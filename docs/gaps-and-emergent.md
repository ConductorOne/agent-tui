---
type: notes
title: "agent-tui — emergent topics + flow gaps surfaced during P-UX implementation"
created: 2026-05-27
inputs: ux-rfc.md, ux-rfc-followups.md
---

# Emergent topics + flow gaps

Things that surfaced while building P-UX1 through P-UX5 + manifest
prototype that weren't in the original RFC. Some are bugs, some are
UX paper cuts, some are real new sub-problems.

Triage at the top, deep dives below.

## 0. TL;DR — top 5 items by impact

1. **First-bytes detection timing** — at spawn, the generic adapter
   wins because no PTY output has arrived yet. The first snapshot
   uses the wrong adapter. Re-detect pass fires later. Currently a
   user-visible bug. (§3.1)

2. **No `--cwd` / `--env` on spawn/run** — every workflow needs
   `bash -c "cd ...; K=V ..."`. Two flags would eliminate that. (§3.2)

3. **Client read timeout hardcoded at 25s** — long-running asks
   (opencode, slow models) hit it before `--max` does. Should be
   per-call configurable; `ask`/`run` already have `--max` that
   doesn't propagate. (§3.3)

4. **No `agent-tui inspect <session>`** — when something fails,
   debugging requires manual cast-file spelunking. A diagnostic
   bundle command would close the gap between "it failed" and
   "here's what happened." (§3.4)

5. **`wait` predicates miss "child is ready for stdin"** — the
   `sleep 1` after `spawn --stdin pipe` is a hack. There's no
   primitive that fires on "child has called read(stdin) ≥1×."
   Pattern shows up in every Mode-A flow that touches stdin. (§3.5)

## 1. Bugs found during the build (mostly fixed)

| # | Bug | Impact | Status |
|---|---|---|---|
| 1.1 | `pipe()` not `pipe2(O_CLOEXEC)` → child inherited daemon's pipe fds, EOF didn't propagate | Showed as "claude hangs on stdin"; took hours to root-cause | Fixed in P-UX2 |
| 1.2 | `dup()` not `F_DUPFD_CLOEXEC` → same problem for slave PTY fds | Same fingerprint | Fixed in P-UX2 |
| 1.3 | Auto-`daemon shutdown` after `run` raced with next `run` | Intermittent "daemon closed without responding" | Worked around by removing auto-shutdown |
| 1.4 | `\n` in `--stdin` taken literally | UX paper cut | Fixed via `interpret_escapes` |
| 1.5 | ANSI-stripper missed 3-byte char-set escapes (`ESC ( B`) | Garbage chars leaked into clean output | Fixed |
| 1.6 | OpenCode hidden title-generation pre-call consumed first Script slot | Tests intermittently failed | Worked around via `--title fixed` |
| 1.7 | OpenCode bash-tool schema requires `description` field not just `command` | Tests passed for wrong reason (marker echoed in args, not output) | Documented |
| 1.8 | SQLite WAL not checkpointed → data invisible to byte-grep on `.db` alone | Tests false-negative'd | Fixed by scanning `.db` + `.db-wal` |

**Pattern.** Half of these are async-fd-lifecycle bugs (CLOEXEC,
shutdown race). The other half are external-CLI quirks (OpenCode
title pre-call, claude requiring pipe-stdin). The first class is
fixable by audit; the second is fixable by recipes (which we now
have via `ask`).

## 2. Emergent topics — themes we didn't predict

### 2.1 Cross-process fd hygiene needs an audit

Once the CLOEXEC bug was found, the question became: where ELSE
might fds leak into children? We have:

- The stdin pipe (fixed)
- Duped slave PTY fds (fixed)
- The master PTY itself — exposed by `try_clone_reader` in the
  reader task. Reader task lives in the daemon process so this is
  fine, but the `Mutex<Box<dyn MasterPty>>` storage could be moved
  by a future refactor.
- The unix-socket listener — used by `interprocess`. Should also be
  CLOEXEC.
- The recorder's write fd for the cast file.
- Lots of debug/log fds in libstd / tokio.

**Action item:** add an integration test that spawns a child and
audits `/proc/<child>/fd/` — assert that ONLY 0/1/2 are open
(plus any explicit inheritance). This catches future regressions.

### 2.2 Recorder casts as a primary debugging surface

Every diagnostic in this session benefited from the `p1.cast` file.
But:
- Cast files have no embedded session metadata (cwd, env, argv)
- No correlation with the command-log
- No "snapshot at event N" markers

A v3 cast format could include sidecar JSON (env + command-log) so
debugging is single-file. Or a `agent-tui inspect <session>` that
collates the cast + command-log + sidecars into a debug bundle.

### 2.3 The 80×24 assumption is everywhere

Default geometry is 80×24. Manifests assume it (`cols = [0, 40]`
for left panels, etc.). Some hosts run terminals at 200+ cols which
TUI apps expect, and manifests stop slicing correctly.

**Two-stage fix:**
- Short-term: document that manifests target 80×24; let user pass
  `--cols/--rows` to spawn to fix the geometry to manifest
  expectations.
- Medium-term: manifest cols use FRACTIONAL or PROPORTIONAL specs
  (e.g., `cols = ["0%", "50%"]`) so layout slicing scales with
  terminal width.

### 2.4 Manifest detection has a timing problem

Adapter `detect()` runs at spawn time. At that point:
- `info.comm` = argv basename (good)
- `info.argv` = full argv (good)
- `info.first_bytes` = empty (the child hasn't written yet)

So banner-regex detection never fires on the initial detect — only
the re-detect pass (~50ms later) sees first-bytes. But by then we
might have already snapshotted with the wrong adapter.

**Action item:** make snapshot lazy — first snapshot deliberately
waits up to N ms for re-detect to settle. Or: make detect() async
and capable of awaiting first-bytes. Or: snapshot returns "adapter
not yet certain" if first_bytes is empty + the adapter scored low.

### 2.5 Sugar-verb recipes are code, not data

The `ask_recipe` function is a hardcoded match in `commands.rs`.
Adding a new provider requires editing Rust + rebuild. The RFC §5
called this out for `ask` specifically; what's emerged is that the
SAME pattern fits for:

- `edit` (per-editor recipes? `helix --select=…`?)
- `watch` (per-tool filtering? `kubectl logs -f` needs specific args)
- Spawn-time governance recipes (allowed-binaries per session)

A repo-shared `recipes.toml` would let the recipes scale like
manifests. Pre-condition: same security model as adapter manifests
(sha256-pin downloads, refuse HTTP, etc.).

### 2.6 Multi-step / conversation continuity

Agent CLIs all have a "continue this session" pattern (claude
`--continue`, opencode `--continue` + `--session`, pi `--resume`).
agent-tui doesn't surface this — every `ask` is fresh.

A natural extension would be:

```bash
agent-tui ask claude "refactor X" --session-id abc
agent-tui ask claude "now add tests"  --session-id abc
```

…where agent-tui tracks the per-provider session-id and passes the
right resume-flag. Could live in the recipe TOML.

### 2.7 Governance / permissions layering

`--allowed-binaries` controls what agent-tui will spawn. But when
agent-tui drives claude, and claude runs bash inside its tool
loop, there's a NESTED permission model:

- Agent-tui's allowlist (does the user trust agent-tui to spawn X?)
- Claude's tool permissions (does claude have permission to run bash?)
- bash's permissions (does the bash command itself need anything?)

Today we stack `--dangerously-skip-permissions` on claude inside an
agent-tui sandbox and hope for the best. There's no unified
"sandbox these K subprocesses" model.

This isn't urgent — but it's a real gap as agent-tui grows.

### 2.8 The error-message hygiene gap

Sample errors observed:

- `daemon read timed out (25s)` → no hint about which op
- `pty spawn failed: spawn child` → no hint about what failed (binary missing? wrong arch? permission?)
- `no such test exists` (from xtask) → fixed once; could recur for any namespacing

A consistent error-shape with `{op, pane, session, hint, context_chain}` would help. We have `ErrorBody { code, message, hint }` already; we should USE it more.

### 2.9 Skills + verbs aren't auto-discoverable from `--help`

A user reading `agent-tui --help` sees the verb list but no signal
that `agent-tui skills get core` exists. The CLI's `about` text
could include "Run `agent-tui skills list` to see workflows."

Tiny change, big effect for first-time users.

### 2.10 The fake-inference server is becoming load-bearing

Originally a test fixture. Now exercised by 8 integration tests.
It's >600 LOC and has accumulated:

- Chat-completions API
- Responses API
- Tool-call events
- Request observability (`server.requests()`)
- Multi-request scripts

Eventually this should be its own published primitive — `cargo
install agent-tui-fake-inference` — so OTHER projects (not just
agent-tui) can drive AI CLIs hermetically in their tests.

## 3. Specific flow gaps (numbered)

### 3.1 First-paint adapter wrong

```bash
agent-tui spawn -- vim file.txt
agent-tui snapshot --mode outline   # adapter: generic (wrong!)
sleep 0.1
agent-tui snapshot --mode outline   # adapter: vim (correct)
```

Workaround: caller knows to wait. Fix: snapshot blocks on
first-byte arrival OR re-detect with a deadline.

### 3.2 No `--cwd` / `--env` on spawn / run

Every workflow that needs a non-default cwd or env vars wraps in
`bash -c "cd X; KEY=VAL ..."`. Two flags on spawn/run would
eliminate ~80% of bash wrappers.

```bash
# Today
agent-tui run -- bash -c "cd /work && PORT=8080 ./serve"
# Desired
agent-tui run --cwd /work --env PORT=8080 -- ./serve
```

### 3.3 Client read timeout is hardcoded

`client.rs` has `timeout(Duration::from_secs(25), ...)` — every
command. `agent-tui ask opencode "..."` waits up to 25s on the wait
call before the client gives up, even though the daemon's
`wait --max` was set to 120000ms.

Fix: derive the client timeout from the request's `--max` plus a
small safety margin.

### 3.4 No diagnostic-bundle command

`agent-tui doctor` exists but is environmental. There's no
"something went wrong, dump everything about this session" command.

```bash
# Desired:
agent-tui inspect --session foo > bundle.tar.gz
# Includes: every cast, every command-log entry, governance
# audit log, daemon log lines from journalctl, recorder stats,
# adapter detection trace.
```

### 3.5 `wait --ready-for-stdin`

The Mode-A pattern is:
```
spawn --stdin pipe
sleep 1                ← workaround
stdin --text "..."
close-stdin
wait --exit
```

The `sleep 1` is there because we don't know if the child has
actually started reading. Some children need a moment; some
panic if we close-stdin before they call read(2).

We have no primitive that fires on "child has called read(stdin) ≥
1×." Implementing this cleanly requires either ptrace-light syscall
tracing (Linux-only, heavy) OR adding a hint to recipes ("this CLI
takes 500ms to start reading"). Recipes are simpler.

### 3.6 `tail --follow` doesn't tee to a file

`tail --follow > /tmp/log.txt` works (shell redirect), but a
built-in `--tee` flag would let agents follow live AND get a
captured file in one call.

### 3.7 Snapshot `--mode text` doesn't preserve color

Strips ANSI; loses color. Some agents want colored output
(presenting to humans, debugging). A `--keep-color` would preserve
SGR while still joining cells into lines.

### 3.8 No way to interrupt a wait without killing the pane

```bash
agent-tui wait --exit --max 60000   # blocks
# User wants to cancel...
```

Today: Ctrl-C the CLI. The wait dies but the daemon is fine. But
there's no "cancel the wait from another invocation" — useful for
scripted timeouts.

### 3.9 Sessions accumulate stale state

After a crash or kill -9, `$XDG_STATE_HOME/agent-tui/<session>/`
keeps the cast files + sidecars. No `agent-tui session gc` to
prune dead sessions older than N days.

### 3.10 Run's exit code surfacing

`agent-tui run -- false` returns exit_code 1 in JSON + exits the
CLI with code 1. Good. But for `--keep-daemon` mode, the daemon
stays alive holding a now-dead pane. List shows it; die clears it.
Friction: callers need to `die` after every `run --keep-daemon`.

Fix: `run --keep-daemon` should auto-die the pane on completion
(the daemon stays, the pane goes).

### 3.11 `ask opencode` is slower than ask claude

Observed: 4-5s for claude, often >25s for opencode (hits client
timeout). Not a bug in agent-tui; opencode is just slow. But the
recipe could surface this:

```bash
agent-tui ask opencode "..." --max 60000   # opt-in to slow path
```

Or auto-pick a higher default for known-slow providers.

### 3.12 Manifest detection vs. Rust adapter precedence

When both a Rust adapter AND a manifest match (e.g., we ship
`htop` as both for some reason), tie-breaking is "registration
order." Today we always register Rust adapters first. But a user
who drops a custom `lazygit.toml` into `~/.config/agent-tui/
adapters/` might want it to OVERRIDE the built-in.

Spec: user-config dir manifests take precedence over built-in.
Currently not implemented.

### 3.13 `--json` flag inconsistencies

Different verbs treat `--json` differently:
- `run` — wraps output in a structured envelope
- `tail --follow` — emits NDJSON per chunk
- `snapshot` — returns the daemon's envelope
- `ask` — pretty by default, JSON via `--json`

Documented in commands.md but the agent-facing default isn't
uniform. Either every verb is JSON-by-default under `--json`, or
the agent recipes spell it out.

### 3.14 Per-CLI banner detection is best-effort

The manifest's `banner_regex` matches against `first_bytes`, but
only the daemon's re-detect pass populates `first_bytes`. If the
agent calls snapshot in the ~50ms window before re-detect, the
manifest doesn't fire.

Same root cause as 3.1; fix unblocks both.

## 4. Cross-cutting tensions

### 4.1 Robustness vs determinism

Real CLIs have:
- Nonzero startup time
- Hidden pre-calls (opencode title-gen)
- Auth flows
- Network jitter

Our timing-based scenarios (`sleep N` and `wait --idle`) are
brittle. The cure is more lifecycle events (`child.spawned`,
`child.first-output`, `child.exited`) and waits that target them.

### 4.2 "Just enough" vs "fully general"

Adapter manifests cover 80% of TUI apps with simple region specs.
But helix, neovim, k9s have layouts that change dynamically
(splits, panes-within-panes, modal overlays). A manifest can't
express these without becoming a programming language.

The pragmatic answer is "manifest for the easy 80%, Rust for the
hard 20%." But we need to be honest in docs about which apps need
which.

### 4.3 Drift policing scope creep

`docs-coverage` + `cli-coverage` started focused on skill/CLI
correspondence. Should they also police:
- Recipe coverage (every ask provider documented?)
- Manifest coverage (every bundled manifest tested?)
- Cast corpus coverage (every documented test has a cast?)

Each is one more enforcer that catches a new class of drift but
adds maintenance load. We should add them only when a real drift
incident happens, not preemptively.

### 4.4 Sessions vs single-shot

The `run` verb is one-shot. The `spawn`+`tail`+`die` pattern is
session-scoped. The daemon is per-session. Three lifetimes
overlap and the user has to keep them straight.

A unified mental model — "every invocation is a single session by
default; `--session foo` opts into shared state" — would help. The
flag exists; the docs don't make it obvious WHEN to use it.

## 5. Recommended next-iteration ordering

If we'd reopen the OODA loop tomorrow, this is the priority I'd
pick:

| # | Item | Why first |
|---|---|---|
| 1 | `--cwd` / `--env` on spawn/run | One-liner. Eliminates the most common bash-wrapper |
| 2 | First-paint adapter fix | Visible bug; affects every adapter test |
| 3 | Configurable client read timeout | Fixes `ask opencode` and other slow-call cases |
| 4 | `agent-tui inspect <session>` | Best ROI for the "next debugging session" we'll inevitably have |
| 5 | Recipes-as-data (extract `ask_recipe` to TOML) | Pre-req for community-contributable AI CLIs |
| 6 | `wait --first-stdin-read` | Removes the `sleep 1` hack and unblocks tighter scenarios |
| 7 | `--keep-color` on snapshot/tail | Small; closes a real human-facing gap |
| 8 | fd-leak audit test | Catches future CLOEXEC regressions before users do |

Items 1, 7, 8 are small (S). 2, 3, 4 are medium. 5, 6 are M-L.
Phasing: ship 1, 2, 3, 7, 8 in one cycle (call it P-UX6); 4, 5,
6 in P-UX7.

## 6. What's NOT a gap (anti-list)

Documented here so we don't re-litigate during planning:

- **Authentication / vault** — parked in main RFC; deferred until
  verb surface is stable. Still the right call.
- **MCP intent surface** — parked; mirrors verbs once stable.
- **Cross-platform parity** — Linux + macOS first per RFC.
  Windows follows.
- **A general programmable shell** — out of scope by design.
- **Replacing expect/pexpect** — explicit non-goal.

## 7. Lessons that should bake into our process

1. **Add a CLOEXEC test before a new fd-allocating code path
   lands.** We didn't have one; we got bitten.
2. **Run the eval against a real CLI before declaring a primitive
   done.** Bash echo isn't enough — claude / opencode flushed out
   bugs that simpler programs hid.
3. **When a scenario passes for the wrong reason** (cycle C marker
   matched via echoed args), surface this as a STRONGER assertion
   spec, not as an OK signal.
4. **Recipes scale better than code-paths.** Both adapter manifests
   and `ask` recipes followed this pattern. Future per-app knobs
   should default to "data, not code."
5. **The cast file is the source of truth.** Every diagnostic
   started by reading it. Invest in its richness (env, command-log
   embedding, structured event types) before investing in new
   debugging tools.
6. **Run `cargo xtask cross-check` BEFORE push, not after CI yells.**
   PR #2 broke macOS + Windows; PR #3 fixed macOS then broke macOS
   AGAIN because rustix's `pipe_with(CLOEXEC)` is Apple-excluded
   despite reading as cross-platform. Each round cost a CI cycle.
   The mechanism (cross-check xtask) was there before PR #3; I
   didn't run it. The lesson is "use the tool you built."
7. **Pre-existing CI failures aren't the new PR's bug, but they
   accumulate technical debt.** PR #1's merge left two integration
   jobs red; PR #2 inherited that; PR #3 inherited it. The cost
   compounds — every PR's CI signal gets noisier and reviewers
   start ignoring CI entirely. Fix red main as a standalone PR.
8. **Test harnesses must drain stderr.** The `Scenario::run_cli`
   docker harness only read stdout. When the agent-tui binary
   silently failed to exec on Alpine (glibc/musl mismatch), the
   only diagnostic the test surfaced was `agent-tui spawn returned
   no stdout`. Adding stderr to the error message immediately
   surfaced the real cause. Any subprocess-driving harness needs
   this from day 1.
9. **Match base-image libc to the build target.** `cargo build`
   on ubuntu-latest produces a glibc binary. Alpine is musl.
   Mounting a glibc binary into an Alpine container = silent
   exec failure (the ELF interpreter doesn't exist). For mounted-
   binary fixtures, use `debian:bookworm-slim` or build with
   `--target x86_64-unknown-linux-musl`. The first is simpler.
10. **Sandbox features that work locally may fail in CI.** bwrap's
    `--unshare-net` brings up loopback inside the new netns —
    needs CAP_NET_ADMIN-shaped privileges that GH Actions runners
    don't grant unprivileged users. Locally we have them; in CI
    we don't. Every sandbox feature needs a CI-friendly escape
    hatch from the start.
