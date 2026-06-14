---
type: rfc
title: "agent-tui — Reference Docs + Skills System"
status: draft
author: Paul Querna
created: 2026-05-27
---

> **Historical design note.** This document predates the public release of
> agent-tui and may not match current behavior. It is kept for design context.

# RFC: `agent-tui` Reference Docs + Skills System

- **Status:** Draft v0
- **Companion to:** `docs/RFC.md` (architecture). This RFC only concerns
  how we **document** what's already designed.
- **Goal:** Stand up a discoverable, agent-friendly, drift-proof
  reference doc system for `agent-tui` modeled on Vercel Labs'
  `agent-browser` skills system, with a systematic guarantee that every
  documented sub-command / use-case has a backing test.

## 0. TL;DR

- **What:** A `skill-data/` tree of versioned skill packages (markdown
  + runnable templates) bundled into the `agent-tui` binary and served
  via the already-stub'd `agent-tui skills get/list` subcommands.
- **Where the docs live:**
  - One thin **discovery stub** outside the binary
    (`~/.claude/skills/agent-tui/SKILL.md`) — points the agent at the
    binary for the actual content.
  - The **canonical content** lives in-repo at
    `crates/agent-tui/skill-data/**` and is embedded via `include_str!`
    so it's version-locked to the binary that ships it.
- **Drift prevention:**
  1. CI parses every documented `agent-tui <subcommand> --<flag>` token
     and asserts it exists in the clap `Cli` enum. Doc-invented commands
     fail the build.
  2. The reverse: every clap subcommand must appear in at least one
     skill page, or it lands on an explicit `undocumented:` allowlist.
  3. Each fenced `bash {test=…}` block carries a back-reference to the
     test that exercises it. A `cargo xtask docs-coverage` check
     enforces those tests exist.
- **Doc↔test mapping:** Each documented "use case" (a `## Heading`
  scope in a skill page) declares `<!-- tested-by: test_fn_name -->`
  immediately under the heading. CI verifies the test exists. New
  use-cases without a `tested-by:` annotation fail lint.

## 1. Motivation

### 1.1 Drift is the dominant failure mode of LLM-facing docs

Skills documents drift faster than human-facing docs because:

- **The reader optimizes against the doc.** When an agent reads
  "agent-tui supports `--foo`," it will invoke `--foo`. A stale doc
  doesn't slow a human reader down; it actively breaks an agent.
- **No human gates each interaction.** A human can re-read the
  command's help text when something fails. An agent will burn
  context retrying.
- **The blast radius is large.** A wrong skill drives wrong actions
  across thousands of sessions until someone notices.

`agent-browser` solves this by **shipping the skills inside the
binary** and exposing them via `agent-browser skills get core`.
Whatever the user's `agent-browser --version` is, the skills it serves
match exactly. The external stub (`~/.claude/skills/agent-browser/
SKILL.md`) is intentionally tiny — it only tells the agent how to
load the real content from the CLI. We adopt the same pattern.

### 1.2 The doc↔test correspondence is not free

Even if drift between docs and the **binary** is solved by embedding,
drift between docs and **reality** is not — a skill page can document
a flow that compiles but no longer works end-to-end. The pi/opencode
integration tests we just built are the strong proof we want: real
fixtures, real binaries, real assertions.

So the second leg is making the doc↔test relationship machine-
checkable, not "vibes-checkable." Every documented use-case gets a
named test. A use-case without a test is a docs bug.

### 1.3 The CLI stubs are already there

`SkillsArgs { action: SkillsAction::{List, Get { name, full }} }`
exists in `crates/agent-tui/src/cli.rs` but has no backing
implementation. There's no `skill-data/` directory yet. This RFC
defines what to fill in.

## 2. Inspiration: how agent-browser does it

Reverse-mapped from the installed package
(`/opt/npm-tools/node_modules/agent-browser`):

```
agent-browser/
├── README.md                              ← human-facing
├── skills/
│   └── agent-browser/SKILL.md             ← thin discovery stub (~50 lines)
└── skill-data/
    ├── core/
    │   ├── SKILL.md                       ← main usage guide (~445 lines)
    │   ├── references/
    │   │   ├── commands.md                (322)
    │   │   ├── snapshot-refs.md           (219)
    │   │   ├── authentication.md          (303)
    │   │   ├── session-management.md      (193)
    │   │   ├── profiling.md               (120)
    │   │   ├── proxy-support.md           (194)
    │   │   └── video-recording.md         (173)
    │   └── templates/
    │       ├── authenticated-session.sh
    │       ├── capture-workflow.sh
    │       └── form-automation.sh
    ├── electron/SKILL.md
    ├── slack/SKILL.md
    ├── dogfood/SKILL.md
    ├── vercel-sandbox/SKILL.md
    └── agentcore/SKILL.md
```

Properties worth copying verbatim:

| Property | Why |
|---|---|
| `core/SKILL.md` as the canonical "start here" | Single entry point; specialized skills branch from it |
| `references/` hold deep dives | Keeps `SKILL.md` short enough to fit in a single agent read |
| `templates/` are runnable scripts | Agents can copy + adapt, not just read |
| Specialized skills as siblings (not children) | Adapter-style: `electron`, `slack`, `dogfood` are separate use-cases |
| The skill content is served by the binary | Single source of truth + version lock |
| The discovery stub is intentionally dumb | Lives outside the binary, doesn't drift because it has nothing to drift from |

Properties we have to **adapt** for the TUI use-case rather than copy:

| Browser concept | TUI analog |
|---|---|
| Page / DOM | PTY screen / engine state |
| `@eN` element ref | `@eN` outline-node ref (same encoding; different source) |
| Navigation (`open URL`) | Process spawn (`spawn <argv>`) |
| `wait --url`/`wait --text`/`wait --load` | `wait --text`/`wait --hash`/`wait --idle`/`wait --sequence` |
| Auth vault | (P3+) per-session credential vault — same shape, different secrets surface |
| Tabs | Panes (same multiplexing primitive, different metaphor) |
| Cross-origin iframes | Tmux/nested-TUI nested adapters |
| Dialogs | Adapter-detected modal states |
| `eval`/JavaScript | Direct PTY input (`type`, `press`, `send-ansi`) |
| `network route` | (out of scope — we don't intermediate network) |

## 3. Proposed layout

### 3.1 Directory tree

```
agent-tui/
├── docs/
│   ├── RFC.md                  ← architecture (unchanged)
│   ├── skills-rfc.md           ← this doc
│   └── windows-strategy.md
├── crates/agent-tui/
│   ├── src/cli.rs              ← already has SkillsAction::{List, Get}
│   ├── src/skills.rs           ← NEW: include_str!ed bundle + dispatch
│   └── skill-data/             ← NEW: canonical content
│       ├── core/
│       │   ├── SKILL.md
│       │   ├── references/
│       │   │   ├── commands.md
│       │   │   ├── snapshot-refs.md
│       │   │   ├── wait-and-events.md
│       │   │   ├── adapter-model.md
│       │   │   ├── policy-and-governance.md
│       │   │   ├── recording.md
│       │   │   └── distribution.md
│       │   └── templates/
│       │       ├── shell-session.sh
│       │       ├── vim-edit.sh
│       │       └── ai-cli-driver.sh
│       ├── shell/SKILL.md
│       ├── vim/SKILL.md
│       ├── claude-code/SKILL.md
│       ├── ai-cli/SKILL.md       ← drives opencode/pi/codex from outside
│       ├── tmux/SKILL.md         ← P3+ (deferred from RFC.md)
│       └── nvim/SKILL.md         ← P4 (deferred from RFC.md)
└── plugin-distribution/
    └── npm/agent-tui/
        └── skills/
            └── agent-tui/SKILL.md  ← discovery stub (~40 lines)
```

### 3.2 SKILL.md frontmatter

Match agent-browser's exact frontmatter shape so existing skill
loaders (Claude Code, etc.) see it as a peer:

```yaml
---
name: <kebab-case>
description: <single sentence used for skill auto-selection>
allowed-tools: Bash(agent-tui:*), Bash(npx agent-tui:*)
hidden: true        # discovery stub only
---
```

### 3.3 What `skills get/list` returns

```
$ agent-tui skills list
NAME           DESCRIPTION
core           agent-tui usage: spawn, snapshot, wait, interact, snapshot @eN refs
shell          POSIX shell sessions with OSC 133 markers
vim            vim/nvim adapter-aware editing
claude-code    driving claude-code itself
ai-cli         driving opencode/pi/codex from outside

$ agent-tui skills get core
<contents of skill-data/core/SKILL.md>

$ agent-tui skills get core --full
<core/SKILL.md + all of references/* + all of templates/*>

$ agent-tui skills get ai-cli
<contents of skill-data/ai-cli/SKILL.md>
```

### 3.4 Discovery stub (ships in npm/cargo install only)

```markdown
---
name: agent-tui
description: Terminal/TUI automation CLI for AI agents. Drive vim,
  shell, claude-code, opencode, or any PTY app via pane snapshots
  and @eN refs.
allowed-tools: Bash(agent-tui:*)
hidden: true
---

# agent-tui

Discovery stub. The real content is served by the CLI:

    agent-tui skills get core           # workflows + common patterns
    agent-tui skills get core --full    # include references + templates
    agent-tui skills list               # see specialized skills
```

The stub never changes. The CLI's content is what changes per release.

## 4. Embedding strategy

### 4.1 Bundle into the binary

```rust
// crates/agent-tui/src/skills.rs (NEW)
struct Skill {
    name: &'static str,
    description: &'static str,
    body: &'static str,
    references: &'static [(&'static str, &'static str)],
    templates: &'static [(&'static str, &'static str)],
}

const CORE: Skill = Skill {
    name: "core",
    description: include_str!("../skill-data/core/_description.txt"),
    body: include_str!("../skill-data/core/SKILL.md"),
    references: &[
        ("commands.md",          include_str!("../skill-data/core/references/commands.md")),
        ("snapshot-refs.md",     include_str!("../skill-data/core/references/snapshot-refs.md")),
        ("wait-and-events.md",   include_str!("../skill-data/core/references/wait-and-events.md")),
        ("adapter-model.md",     include_str!("../skill-data/core/references/adapter-model.md")),
        ("policy-and-governance.md", include_str!("../skill-data/core/references/policy-and-governance.md")),
        ("recording.md",         include_str!("../skill-data/core/references/recording.md")),
        ("distribution.md",      include_str!("../skill-data/core/references/distribution.md")),
    ],
    templates: &[
        ("shell-session.sh", include_str!("../skill-data/core/templates/shell-session.sh")),
        ("vim-edit.sh",      include_str!("../skill-data/core/templates/vim-edit.sh")),
        ("ai-cli-driver.sh", include_str!("../skill-data/core/templates/ai-cli-driver.sh")),
    ],
};

pub const ALL_SKILLS: &[&Skill] = &[&CORE, &SHELL, &VIM, &CLAUDE_CODE, &AI_CLI];
```

The `_description.txt` sidecar holds the single-line description used
in `skills list`, kept separate so it can be grep'd without parsing
the markdown frontmatter.

### 4.2 Dispatch

`SkillsAction::List` → walk `ALL_SKILLS`, print `name + description`.

`SkillsAction::Get { name, full: false }` → emit `body`.

`SkillsAction::Get { name, full: true }` → emit `body` + a separator
then each reference + each template. agent-browser uses a simple
form-feed `\f` separator; we can do the same or use a Markdown
horizontal rule with a unique-nonced marker (so the agent can split it
back cleanly).

## 5. Drift prevention

Three classes of drift; one mitigation each.

### 5.1 Doc → CLI drift

**Failure mode:** a skill page documents `agent-tui spawn --foo` but
`--foo` was renamed to `--bar`.

**Mitigation:** `cargo xtask docs-coverage` parses every fenced
`bash` block, finds every `agent-tui <subcmd> [args…]` invocation,
parses it through the same clap `Cli` definition used by the binary.
Errors fail CI.

The same xtask records a normalized list of (subcommand, flag) tuples
referenced anywhere in `skill-data/**`. Mismatches with the clap
schema fail CI.

### 5.2 CLI → doc drift

**Failure mode:** a new subcommand or flag lands in `cli.rs` but no
skill page mentions it. Agents won't discover it.

**Mitigation:** the same xtask walks the clap `Cli` enum and collects
every reachable (subcommand, flag). Any item not referenced by at
least one `skill-data/**/*.md` must appear on
`skill-data/.undocumented-allowlist.txt`. New CLI surface without
either docs **or** an explicit allowlist entry fails CI.

The allowlist mechanism gives us an explicit way to ship a flag
without docs (e.g. an experimental flag) while still failing-closed
by default.

### 5.3 Doc → behavior drift

**Failure mode:** the skill page documents a workflow that worked at
authoring time but no longer works end-to-end.

**Mitigation:** see §6.

## 6. Systematic doc↔test mapping

### 6.1 The `tested-by:` annotation

Every `##` heading in a skill page that documents a use-case declares
which test exercises it:

```markdown
## Spawning a vim session and reading the buffer
<!-- tested-by: vim_bwrap::bwrap_vim_opens_file_and_shows_content -->

```bash {test=vim-open-and-snapshot}
agent-tui spawn -- vim /work/notes.md
agent-tui snapshot --mode outline
```
```

The `<!-- tested-by: <module>::<fn> -->` HTML comment is a structured
annotation. The fenced-block `{test=…}` tag is the local handle so
the same heading can have multiple block→test mappings.

### 6.2 The coverage enforcer

`cargo xtask docs-coverage` (run in CI):

1. Walks `skill-data/**/*.md`.
2. For each `##` heading, asserts a `<!-- tested-by: … -->` immediately
   follows. If missing, fail with the heading path.
3. For each referenced `<module>::<fn>`, asserts the test target
   exists by running:

   ```
   cargo test -p agent-tui-integration --list -- --format=terse
   ```

   …and grep'ing the listed names. (`--list` doesn't execute.)
4. Reports any documented test that doesn't exist + any test in
   `tests/` that no doc claims.

Output looks like:

```
docs-coverage: PASS  (24 use-cases, 24 tests, 0 orphans)

— or —

docs-coverage: FAIL
  ✗ core/SKILL.md:142 "Waiting for command completion"
    no `<!-- tested-by: … -->` annotation found.
  ✗ vim/SKILL.md:38 declares tested-by `vim_bwrap::bwrap_vim_undo`
    but that test does not exist (did you rename it?).
  ⚠ test `vim_bwrap::bwrap_vim_search_finds_target` is not
    referenced from any doc (orphan; intentional? add to
    .undocumented-tests.txt).
```

### 6.3 Why HTML comments instead of YAML?

Tried YAML front-matter for use-cases; ended up with:
- Either a heavy block of YAML between every heading (noisy), or
- A separate sidecar manifest file that drifts from the markdown.

HTML comments are invisible in any markdown renderer, are tied to the
heading they document, and machine-grep cleanly with a single regex.

### 6.4 Tested-by on bash blocks vs. headings

We use **two** levels:

| Level | Granularity | Purpose |
|---|---|---|
| Heading (`<!-- tested-by: m::fn -->`) | Use case | "This workflow has integration coverage" |
| Block (`bash {test=label}`) | Individual snippet | The label lets multiple blocks under one heading map to multiple sub-tests if needed |

Most use-cases will have one block ≈ one test ≈ one heading. The
multi-block form is reserved for cases like "log in, then list, then
delete" where each step deserves a separate test.

### 6.5 What gets enforced vs. what's a soft check

| Check | Hard fail? |
|---|---|
| `bash` block invokes a flag that doesn't exist in clap | yes |
| `##` heading without `tested-by:` annotation | yes |
| `tested-by:` references a test that doesn't exist | yes |
| Test exists but no doc claims it | warning (orphan list) |
| Subcommand exists but no skill page mentions it | yes (unless allowlist) |
| Flag spelling matches but `--help` text drift | no (separate doc) |

## 7. Initial skill set

Ship `core` + four specialized skills in v0.1:

### 7.1 `core` — the canonical entry point

Sections (each backed by an integration test):

| Section | Tested by |
|---|---|
| The core loop (spawn / snapshot / interact / wait) | `vim_bwrap::bwrap_vim_opens_file_and_shows_content` |
| Reading a pane (`snapshot --mode outline\|cells\|adapter\|hybrid`) | `vim_bwrap::*` + `shell_osc133_bwrap::*` |
| Interacting (`press`, `type`, `send-ansi`) | `vim_adapter_bwrap::*` |
| Waiting (`wait --text/--hash/--idle/--sequence`) | `fzf_bwrap::*` |
| Process lifecycle (`spawn`/`die`/`signal`) | `htop_bwrap::*` |
| Snapshot refs (`@eN` lifetime) | `vim_adapter_bwrap::vim_modified_file_marks_status_node` |
| Multiple panes (focus, list) | `mcp_drives_vim_bwrap::*` |
| Recorder + replay (`*.cast`) | `vimtutor_bwrap::*` (recording artifact) |
| Doctor + diagnosing install issues | `alpine_smoke::*` |
| When to load another skill | (navigation only — no test) |

References:

- `commands.md` — every subcommand, flag, and alias, generated from
  the clap definitions via `agent-tui --help` + xtask
- `snapshot-refs.md` — deep dive on the four snapshot modes, the
  hashing rules, and ref lifetime
- `wait-and-events.md` — sequence numbers, idle vs. text vs. hash
- `adapter-model.md` — generic, shell, claude-code, vim, …
- `policy-and-governance.md` — allowlist, audit events
- `recording.md` — asciicast v3 extensions, rotation
- `distribution.md` — npm/brew/cargo install paths

Templates:

- `shell-session.sh` — open shell, run a command, snapshot result
- `vim-edit.sh` — open vim, edit, save, exit, snapshot
- `ai-cli-driver.sh` — drive opencode/pi via the harness

### 7.2 `shell` — POSIX shell with OSC 133

Targeted at the most common entry-point: driving a bash/zsh session
and detecting prompts vs. running commands. Backed by
`shell_osc133_bwrap`.

### 7.3 `vim` — vim/nvim adapter-aware

Open file, edit, save, search, command-mode. Backed by `vim_bwrap` +
`vim_adapter_bwrap`.

### 7.4 `claude-code` — driving claude-code itself

The reflexive use-case: claude-code driving claude-code. Backed by
`mcp_drives_vim_bwrap` and (TBD) a `claude-code_bwrap` test set.

### 7.5 `ai-cli` — drive opencode/pi/codex

The freshly-built integration coverage. Backed by
`opencode_fake_inference` + `pi_fake_inference`. Documents the
fake-inference pattern as a debugging tool.

## 8. Roll-out phases

### Phase 1 — wire the plumbing (no content yet)

- `crates/agent-tui/src/skills.rs` with `include_str!` machinery
- Stub `skill-data/core/SKILL.md` with a single section
- `cargo xtask docs-coverage` with the heading-annotation check
- `cargo xtask docs-coverage` runs in CI; passes trivially with one
  test backing one section

### Phase 2 — port what already works to skills

- `core` SKILL.md migration from RFC.md (parts of §4 + §5)
- Reference docs: `commands.md` autogenerated from `--help`
- `snapshot-refs.md`, `wait-and-events.md` reference docs
- Backfill `tested-by:` annotations from existing
  `crates/agent-tui-integration/tests/**`
- xtask: clap surface → docs reverse check on

### Phase 3 — specialized skills

- `shell`, `vim`, `claude-code`, `ai-cli` SKILL.md files
- Templates under `core/templates/`

### Phase 4 — discovery stub + distribution

- The thin stub under `plugin-distribution/npm/agent-tui/skills/`
  that the npm package installs to `~/.claude/skills/agent-tui/`
- Same for the brew formula and `cargo install` postinstall

## 9. Open questions

- **Should `skills get core --full` return one big stream or
  separate files?** agent-browser uses a single stream with `\f`
  separators. Easier for an agent to ingest, but harder to selectively
  reference. Tentative: single stream with named-section markers.
- **Where do release notes live?** agent-browser ships a `CHANGELOG.md`
  outside the skills. We probably want the same — release notes are
  for humans, skill content is for agents.
- **How do we test the `--annotate` snapshot mode in docs?** It
  produces a PNG; the docs would reference one but we can't easily
  diff PNGs in CI. Probably: doc shows the command, test asserts the
  PNG file exists with non-zero size.
- **Auth/secrets references** are deferred until the
  agent-tui auth vault lands (P3+ in RFC.md). Stub the topic with a
  pointer to the architecture RFC.
- **MCP vs. CLI parity.** agent-tui has both `mcp serve` and the
  direct CLI. Do we document both surfaces side-by-side in `core` or
  split into `core` (CLI) + `mcp` (MCP)? Tentative: split. Most
  agents using MCP don't see CLI commands at all.

## 10. Decision points to confirm before phase 1

1. **Layout location.** `crates/agent-tui/skill-data/` (inside the
   binary crate) vs. `crates/agent-tui-skills/` (separate crate)?
   Tentative: inside the binary crate. One crate, one `include_str!`
   tree.
2. **xtask vs. integration test.** `cargo xtask docs-coverage` (new
   xtask crate) vs. a `#[test]` inside `agent-tui-integration` that
   walks `skill-data/`. Tentative: xtask. The check is build-time, not
   a workflow test; xtask gives a cleaner CLI for running locally.
3. **Annotation format.** `<!-- tested-by: m::fn -->` vs. a
   front-matter block at the top of each skill. Tentative: HTML
   comments per-heading.
4. **What goes in the stub.** Just the `skills get core` pointer, or
   also a tiny version-mismatch note ("if `agent-tui --version`
   differs from this stub's version, prefer the CLI")? Tentative:
   pointer only, no version concern in the stub.

## 11. Non-goals

- **Human-facing tutorials.** This system targets agents. A separate
  `README.md` / `docs/getting-started.md` can live for humans.
- **API stability guarantees for skill format.** Skills are bound to
  the binary version. We can change the schema between releases.
- **Localization / translation.** English only.
- **Live MCP-served skill content.** Skills are static. If the MCP
  server wants to expose them, it `include_str!`s the same data.

## 12. Appendix: what the first commit looks like

To make the RFC concrete, the first commit lands:

```
docs/skills-rfc.md                                      (this file)
crates/agent-tui/src/skills.rs                          (new: 60 LOC)
crates/agent-tui/skill-data/core/SKILL.md               (new: stub with 1 heading)
crates/agent-tui/skill-data/core/_description.txt       (new: 1 line)
crates/agent-tui/skill-data/.undocumented-allowlist.txt (new: empty)
crates/xtask/Cargo.toml                                 (new)
crates/xtask/src/main.rs                                (new: docs-coverage)
.github/workflows/docs-coverage.yml                     (new)
```

End-to-end: `agent-tui skills get core` prints the stub; CI fails if
the stub's `## Quickstart` heading loses its `tested-by:` annotation
or if the named test is renamed.
