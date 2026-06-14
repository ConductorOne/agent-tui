---
type: rfc
title: "agent-tui — Addressing Model (refs, hierarchy, selectors, targeted writes)"
status: draft
author: Paul Querna
created: 2026-05-28
---

> **Historical design note.** This document predates the public release of
> agent-tui and may not match current behavior. It is kept for design context.

# RFC: agent-tui — Addressing Model

- **Status:** Draft v3
- **Companion to:** `docs/RFC.md` (architecture v3), `docs/ux-rfc.md`
- **Trigger:** Planning tmux + nested-TUI coverage exposed that the
  pane *content* layer is fine but the *addressing* layer can't say
  "the buffer inside pane 2" or "send these keys to the tmux command
  bar." The DOM analogy holds — we have nodes, we lack a query and
  routing surface.

## 0. TL;DR

The protocol already has the bones of a DOM: `OutlineNode` has
`children`, `RefBinding::Durable { scheme, id }` survives re-renders,
and dotted refs (`@e3.4`) work today. What's missing is:

1. **Hierarchy contract** — adapters are free to emit flat outlines
   today and they do. We need a convention (and a tmux adapter that
   honors it) so consumers can rely on the tree shape.
2. **Stable identity** — `@e1`...`@eN` are regenerated per snapshot
   from a numeric counter. Two snapshots of the same pane can refer to
   "the same buffer" with different refs. Selectors and `wait --ref`
   become unreliable.
3. **Selectors** — no way to say "find the focused buffer" or
   "find any status node whose name matches `~written`." Agents must
   walk the outline themselves.
4. **Targeted writes** — `press` writes raw bytes to the outer PTY.
   For tmux that means "always to the focused pane via prefix
   semantics," which is wrong. We need `press --to @ref` with the
   adapter doing the routing.

This RFC proposes:

- A **ref grammar** (`@<root>[.<child>]*`) with adapter-defined
  stable-ID semantics per scheme.
- A **CSS-subset selector** (`role=...`, `name~=/regex/`,
  `focused`, descendant `>`) usable in `snapshot --select` and
  `wait --ref`.
- A new `Adapter::route` method that lets adapters translate
  `(ref, keys)` into the actual byte sequence to write.
- A **tmux adapter** as the proof point — it's the case that forces
  every gap to close.

Concrete shape: ~600 LOC of adapter + 200 LOC of selector parser +
~150 LOC of CLI wiring. Two weeks if it goes well.

## 1. Where we are today

### 1.1 What the protocol already supports

```rust
pub struct OutlineNode {
    pub r#ref: String,                  // @e3 or @e3.4
    pub role: String,                   // "buffer", "status", "pane", ...
    pub name: String,                   // accessible name
    pub value: Option<String>,
    pub focused: bool,
    pub anchor: Option<(u16, u16)>,
    pub children: Vec<OutlineNode>,     // hierarchy is in the wire format
}

pub enum RefBinding {
    Durable { scheme: String, id: Value },  // stable across frames
    Generic { row: u16, col: u16, role: String },  // positional, frame-local
}
```

So:

- **Hierarchy** — `children` exists; no adapter populates it
  meaningfully. The vim adapter emits four sibling nodes (`mode`,
  `file`, `status`, `buffer`) at the top level even though they belong
  to a single buffer view.
- **Stable identity** — `RefBinding::Durable { scheme: "nvim.buffer",
  id: 12 }` is the explicit design. No built-in adapter currently
  uses it. All built-ins return `Generic` bindings.
- **Selectors** — the protocol has no notion. Consumers walk the
  tree by hand.
- **Targeted writes** — `press`/`type` go to the pane's PTY. The
  daemon has one pane per child, so "which pane" *is* "which agent-tui
  pane", not "which sub-pane of tmux."

### 1.2 Why the gap matters now

For single-process TUIs (vim, htop, lazygit), flat outline + generic
refs are fine — the agent's mental model is "one pane = one app."
For composed environments (tmux with N apps, screen, k9s with focused
list rows), the agent needs to point at sub-regions. tmux is the
forcing function because it ships in every dev environment and is
the gateway to nested vim, nested htop, etc.

## 2. Proposal

### 2.1 Ref grammar

Refs are a single dotted path with a scheme-defined head:

```
ref       = "@" head ( "." segment )*
head      = ident                   ; adapter-chosen root, e.g. "tmux", "vim"
segment   = ident
          | ident "[" key "]"       ; named collection, indexed slot
key       = ident                   ; "main", "log"
          | integer                 ; positional, ADAPTER-LOCAL semantics
          | "%" integer             ; conventional stable-id marker
          | "$" ident               ; binding-local symbol
```

Examples:

```
@tmux                       root tmux session
@tmux.pane[%2]              tmux pane with stable id %2 (across renames)
@tmux.pane[1]               tmux pane at window position 1 (NOT stable)
@tmux.cmdline               tmux's own ":" command bar
@vim.buffer[1]              vim buffer with bufnr 1
@vim.statusline
```

**Index semantics are scheme-local.** Two conventions:

- **`[%N]`** — adapter-defined stable identifier (e.g. tmux's pane id
  `%2`, vim's bufnr). Refs with `%` keys MUST come with a
  `RefBinding::Durable { scheme, id }` and MUST persist across
  snapshots until the underlying object is destroyed.
- **`[N]`** — positional. Same logical object may move between
  snapshots. Refs with bare-integer keys MUST come with
  `RefBinding::Generic` so consumers know not to trust them.

The grammar doesn't enforce this — it's an adapter contract. The
selector layer treats `[%2]` and `[2]` as syntactically identical
predicates; the *semantic difference* is conveyed by the ref's
`RefBinding`.

**Why dotted paths and not pure IDs:**

- Human-readable in logs and recipe files.
- Composable: `@tmux.pane[%2]` is a *prefix* of
  `@tmux.pane[%2].buffer`, which lets selectors filter by parent
  without a separate parent pointer.
- Cheaper to diff across snapshots than positional anchors.

**Stability rule:** stability is communicated entirely through
`RefBinding`. `Durable` bindings persist; `Generic` bindings don't.
This already exists in the protocol — we are not adding a new
property, we are documenting that adapters MUST set it correctly.

### 2.1.1 Scheme registry

`RefBinding::Durable.scheme` is open-ended today. To keep cross-adapter
behavior consistent we introduce a registry (`docs/schemes.md`)
listing well-known schemes:

| Scheme              | ID shape | Notes                                |
|---------------------|----------|--------------------------------------|
| `tmux.pane.id`      | `%N`     | tmux pane id from `display-message`. |
| `tmux.window.id`    | `@N`     | tmux window id.                      |
| `vim.buffer.bufnr`  | integer  | vim `bufnr`.                         |
| `vim.window.winnr`  | integer  | vim `winnr`.                         |
| `nvim.buffer.bufnr` | integer  | nvim equivalent.                     |
| `claude.input`      | (none)   | Claude Code prompt row.              |
| `claude.response`   | (none)   | Claude Code scrollback above prompt. |
| `codex.input`       | (none)   | Codex prompt row.                    |
| `codex.response`    | (none)   | Codex scrollback above prompt.       |
| `pi.input`          | (none)   | Pi prompt row.                       |
| `pi.response`       | (none)   | Pi scrollback above prompt.          |
| `aider.input`       | (none)   | Aider prompt row.                    |
| `aider.response`    | (none)   | Aider scrollback above prompt.       |
| `opencode.input`    | (none)   | OpenCode prompt row.                 |
| `opencode.response` | (none)   | OpenCode scrollback above prompt.    |
| `generic.path`      | string   | adapter-defined; not portable.       |

Adapters MAY invent new schemes; they SHOULD prefix with the adapter
name. Consumers MUST treat unknown schemes as opaque.

### 2.2 Selectors

CSS-subset, evaluated against the outline tree:

```
selector  = step ( combinator step )*
combinator= S+ (descendant) | S* ">" S* (direct child)
step      = ( ref_path | "*" ) predicate*
ref_path  = "@" head ( "." segment )*       ; matches a ref path prefix
predicate = "[" attr op value "]"
attr      = "role" | "name" | "value"
op        = "=" | "~=" | "^=" | "$="        ; eq, regex, prefix, suffix
value     = quoted | bareword | "/" regex "/"
predicate = "[focused]" | "[focused=" bool "]"
          | "[durable]"                     ; binding kind filter
```

**Tokenization rule** (resolves the `@tmux pane[%2]` vs
`@tmux.pane[%2]` ambiguity):

- Dots inside a `ref_path` token bind tighter than whitespace.
- Whitespace between tokens is the descendant combinator.
- `@tmux.pane[%2]` is one ref_path step matching a node whose ref
  starts with that path.
- `@tmux pane[%2]` is two steps: any `@tmux` ancestor, then any
  descendant whose last segment matches `pane[%2]`.

Examples:

```
@tmux.pane[%2] > [role=buffer]              one specific buffer (direct child)
[role=buffer][focused]                      the focused buffer, anywhere
[role=status][name~=/written/]              any status node matching
@vim > [role=mode][value=insert]            insert-mode signal
[role=pane] [role=buffer][focused]          focused buffer in any pane
[role=cmdline][durable]                     only durable-bound cmdlines
```

**Error model.** Three failure modes:

- *Parse error* — malformed selector. Returned synchronously from
  the `snapshot --select` / `wait --ref` call with exit code 64
  (EX_USAGE) and a parser-pointer in the message.
- *No match* — well-formed selector matched zero nodes. `snapshot
  --select` exits 0 with `data: null`. `wait --ref` blocks until the
  timeout (existing `--max`) and exits 124 (timeout) like other
  wait conditions.
- *Multi-match where one is required* — `--select` returns first;
  `--select --all` returns all; `wait --ref` doesn't care.

API surfaces:

- `snapshot --select '<selector>' [--all]` — filtered outline.
- `wait --ref '<selector>' [--gone]` — block until match exists, or
  with `--gone`, until no node matches (useful for "wait for the
  confirm to dismiss").
- Library function `protocol::select(&outline, &compiled_selector)`.

Selectors compile once and run against each snapshot's outline tree.
No XPath, no `:nth-of-type`, no pseudo-classes beyond `[focused]`,
`[durable]`. Walking outline trees is O(N) in nodes; the polling
loop in `wait --ref` re-walks on each engine update, not on a fixed
tick.

### 2.3 Targeted writes

The `Adapter` trait gains:

```rust
async fn route(
    &self,
    snap: &EngineSnapshot,
    target: &Ref,
    keys: &EncodedKeys,
) -> Result<Vec<RoutedStep>, AdapterError>;

pub enum RoutedStep {
    /// Write these bytes to the PTY.
    Write(Vec<u8>),
    /// Don't continue until the outline contains a node matching this
    /// selector — bounded by `max_wait_ms`. If the gate doesn't fire,
    /// the routing aborts and `press` returns a `RoutingGateTimeout`
    /// error.
    WaitFor { selector: String, max_wait_ms: u32 },
    /// Coarse-grained fallback when no observable signal exists.
    /// Strongly discouraged; included only for the bootstrap tmux
    /// adapter that doesn't yet know how to detect "border highlight
    /// moved to pane N."
    Delay { ms: u32 },
}
```

Why a `WaitFor` step matters: fixed delays are fragile across
runtimes (CI is slow, local is fast). The adapter knows what
observable change should happen between bytes — encode that
explicitly. `Delay` exists only as an escape hatch and SHOULD be
removed from each adapter once a real `WaitFor` exists.

The daemon's `press` handler:

1. Resolves the selector or literal ref to a node.
2. Asks the attached adapter for `route(target, keys)`.
3. Executes each step in order: `Write` writes to the PTY, `WaitFor`
   reuses the existing wait infrastructure, `Delay` sleeps.

**Adapter-less target error.** If `--to` is supplied but the attached
adapter doesn't recognize the ref's head, the daemon returns
`RoutingUnsupported { ref, adapter }` rather than silently writing.

CLI:

```
agent-tui press --to '@tmux.pane[%2]' 'ihello<esc>'
agent-tui press --to '[role=buffer][focused]' ':w<cr>'
```

Default (no `--to`): unchanged — bytes hit the PTY directly, as
today. This keeps every existing recipe working.

The generic adapter's `route` is the identity: it returns one `Write`
step equal to `keys`, regardless of target (but only matches if the
ref is generic — any structured target falls through to
`RoutingUnsupported`).

For tmux specifically, route emits:

```
press --to '@tmux.pane[%2]' 'ihello<esc>'
→  Write([<prefix>, q]),                     # enter display-panes
   WaitFor { selector: "[role=pane-picker]",
             max_wait_ms: 500 },             # tmux paints overlay
   Write([2]),                               # select pane 2
   WaitFor { selector: "@tmux.pane[%2][focused]",
             max_wait_ms: 500 },             # focus moved
   Write([i, h, e, l, l, o, ESC]),           # the real keys
```

### 2.3.1 Recipes + routing

Adapter manifests (`*.toml`) already declare an adapter's CLI bindings.
Manifests gain an optional `[routing]` block:

```toml
[routing]
# Per-target-prefix recipe for translating keys into bytes.
[[routing.rule]]
match    = "@tmux.pane[%*]"           # match any pane id
steps    = [
    { write = "{prefix} q" },
    { wait_for = "[role=pane-picker]", max_wait_ms = 500 },
    { write = "{pane_index_in_window}" },
    { wait_for = "@tmux.pane[{pane_id}][focused]", max_wait_ms = 500 },
    { write = "{keys}" },
]
```

This lets non-trivial routing live next to the CLI's adapter
declaration rather than in built-in Rust. For v1 the tmux adapter is
Rust; manifests-driven routing is a v2 deliverable.

### 2.4 The tmux adapter (proof point)

What the tmux adapter has to do that no current adapter does:

- **Discover panes** without depending on tmux's control mode RPC.
  v1: scrape the status bar + pane borders by parsing pane numbers
  printed via `display-panes-active-colour` style markers. v2: opt
  into tmux `-CC` control mode for first-class introspection.
- **Emit a real tree.** Top-level node `@tmux`, children
  `@tmux.pane[N]` each with a child `@tmux.pane[N].buffer` whose
  `name` is the visible cell sub-region of that pane.
- **Detect modal overlays.** When tmux opens `:`, `?`, `choose-tree`,
  emit a sibling node `@tmux.modal` whose role is `confirm` /
  `prompt` / `list` etc. The classifier reads this and reports the
  pane state correctly (a `confirm` overlaid on an alt-screen TUI).
- **Honor stability.** `@tmux.pane[2]` stays `@tmux.pane[2]` from
  frame to frame as long as that tmux pane exists.

The tmux adapter is also the proof that we got the four pieces above
right — it can't be built without all four.

## 3. Migration & backwards compat

- Existing flat-outline adapters (vim, htop, lazygit, shell, generic)
  keep working unchanged. Selectors degrade gracefully on a flat
  tree (a `>` direct-child selector simply won't match deeper than
  the surface, which is correct).
- Existing recipes that call `press` without `--to` are unchanged.
- The `route` method needs a default impl on the trait that returns
  one `RoutedBytes` chunk equal to `keys`. Adapters opt in.
- The CLI's selector flag is additive.

### 2.4 Adapter composition (the load-bearing scoping decision)

When the tmux adapter wins detection on the outer pane, *vim
running inside one of tmux's panes* still needs the vim adapter to
emit meaningful outline nodes. Today adapters are one-per-pane;
they don't compose.

**v1 scope:** the tmux adapter emits a flat node per pane with the
raw rendered cell text as `name`. It does NOT recursively classify
what's inside. Selectors like `@tmux.pane[%0] [role=buffer]` will
match the tmux adapter's own `buffer` node (one per pane), not a
nested vim-adapter buffer.

```
@tmux
  @tmux.pane[%0]                    role=pane
    @tmux.pane[%0].buffer           role=buffer, name=<raw text>
  @tmux.pane[%1]
    @tmux.pane[%1].buffer
```

This is a real limitation — it means `wait --ref '@vim.statusline'`
inside a tmux pane won't fire. But it ships in 2 weeks instead of
6, and gets us the addressing model in agents' hands.

**v2 (separate RFC):** introduce adapter composition. The trait
gains a `compose_subtree` method that takes a clipped grid and
returns nodes to graft under a parent ref. The tmux adapter calls
the daemon's adapter registry to detect-and-render each pane
sub-region. Open design questions:

- How does the engine state (cursor, alt-screen flag) of a sub-pane
  get derived? Sub-panes don't have their own alacritty grid; the
  tmux adapter would have to synthesize one.
- Do we keep per-sub-pane sequence numbers? Probably yes, so
  `wait --ref` inside a sub-pane doesn't get fooled by outer-pane
  redraws.

These are real and large; deferred deliberately.

### 2.5 Cross-pane focus (resolved)

The adapter is the source of truth for which sub-node is focused.
`OutlineNode.focused` is set by the adapter; the daemon does not
infer it. The classifier (`PaneState`) reports the outer-most state
(`AltScreenTui` for tmux); per-sub-pane state is exposed via a new
optional `OutlineNode.state` field carrying the same `PaneState`
enum.

Concretely: an agent asking "what's the focused pane doing" walks
the outline to the focused node and reads its `state`. This avoids a
top-level state explosion ("AltScreenTui-with-nested-Editor") while
giving precise info where it matters.

### 2.6 MCP exposure

The MCP server (`crates/agent-tui/src/mcp.rs`) tool descriptions add:

- `wait` gains a `ref` parameter parallel to `text`/`idle`/etc., with
  a string `selector` and optional `gone: bool`.
- `snapshot` gains a `select` parameter and `all: bool`.
- `press` and `type` gain a `to` parameter (selector string).

JSON schema is mechanically updated; behavior is documented in the
tool description. No new MCP tool is needed.

## 4. Open questions

- **Snapshots inside subtrees.** Should `snapshot --select` return the
  full snapshot envelope but with `outline` filtered, or a new
  sub-snapshot type? Filtering is simpler; sub-snapshots are more
  useful for cell-mode renders (return only the cells in the
  matched node's anchor region). Lean toward filtering for v1;
  cell-cropping is a v2 add.
- **Selector + non-outline modes.** If `snapshot --mode cells
  --select X` is called, the daemon has no outline to match against.
  Auto-promote `--select` to also emit outline (still cheap because
  the engine snapshot is the same) and return both `cells` and
  `outline` even when `--mode cells` was nominally requested.
  Document this as "selector forces outline."
- **Tmux prefix detection.** Tmux's prefix is user-configurable
  (default `C-b`, common override `C-a`). The tmux adapter needs
  to know what to emit during routing. v1: read from
  `AGENT_TUI_TMUX_PREFIX` env (default `C-b`). v2: run
  `tmux show-options -g prefix` once at attach time.
- **OSC 1337/52 + bracketed paste interaction.** tmux passes through
  some sequences and rewrites others. `press --to` of a literal
  paste-bracket may not survive tmux passthrough. Likely out of
  scope for v1 — document the gotcha, add a `--raw` flag if agents
  hit it in practice.
- **Routing for non-multiplexed adapters.** Does it ever make sense
  for vim's adapter to route — e.g. `press --to '@vim.buffer[3]'
  'iX<esc>'` translating to `:b 3<cr>iX<esc>`? Probably yes; the
  trait supports it. v1 adapters keep `route` at default.
- **Selector caching.** If a recipe re-uses the same selector in
  many `wait --ref` calls, do we want a "compile this once" API?
  Trivial to add; not pressing.
- **WaitFor gate liveness.** A `WaitFor` step gates on a selector
  appearing, not on it staying live. tmux's pane-picker overlay
  could be dismissed between `WaitFor` firing and the next `Write`
  landing. Probably tolerable in practice; if not, extend `WaitFor`
  with `stable_ms`.

## 5. Out of scope (call out so future-me doesn't drift)

- XPath, `:nth`, advanced CSS pseudos.
- Stable identity that survives daemon restarts. Refs are stable
  *within a session*; across daemons they're whatever the adapter
  re-derives.
- Drag-select / mouse-region addressing. agent-tui doesn't drive
  mouse coordinates today and adding "click @ref" multiplies surface
  area faster than it pays off.
- Cross-pane refs (a ref in pane A pointing at pane B). The daemon's
  pane model is one tree per child process; tmux happens to multiplex
  that tree, but that's an in-adapter concern.

## 6. Test plan

- Unit tests on the selector parser against the grammar
  (every predicate kind, combinator, escape rule, ambiguity case
  `@tmux.pane[%2]` vs `@tmux pane[%2]`).
- Unit tests on `protocol::select` against hand-built outline
  fixtures, including the empty-outline and no-match cases.
- Unit tests on adapter `route` for the tmux adapter using golden
  byte sequences (one per supported tmux key family: send-keys,
  paste-buffer, pane-select).
- Integration: new `tests/tmux_basic.rs` covering
  (a) spawn `tmux new -s s` + vim in pane[%0],
  (b) `press --to '@tmux.pane[%0]' 'iX<esc>'`,
  (c) `wait --ref '[role=pane][name~=/X/]'` (the v1 limitation
      means we wait on tmux's buffer text, not vim's statusline),
  (d) `snapshot --select '@tmux.pane[%0]'`.
- Integration: `tests/tmux_modal.rs` covering
  (a) spawn tmux, open `:` command bar,
  (b) `wait --ref '@tmux.cmdline[focused]'`,
  (c) `press --to '@tmux.cmdline' 'kill-window<cr>'`,
  (d) `wait --ref '@tmux.cmdline[focused]' --gone`.
- Cross-check: bwrap backend, same scenarios.
- Negative tests: malformed selector → exit 64; selector against
  cell-mode-only snapshot → forced outline + warning; routing
  with unsupported adapter → `RoutingUnsupported` error.

## 7. Worked use cases (stress test against existing scenarios)

The model isn't useful if it makes simple things harder. This section
walks every existing fixture through the new addressing model and
asks: does it still read cleanly? Where does friction land?

### 7.1 vim — open, edit, save (`vim_basic.rs`)

**Today:**
```bash
spawn -- vim /work/notes.md
wait --text "notes.md"
press "i hello<esc>"
wait --text "\[\+\]"
press ":w<cr>"
wait --text "written"
snapshot              # then walk outline to find statusline
press ":q<cr>"
```

**With addressing model:**
```bash
spawn -- vim /work/notes.md
wait --ref '@vim.buffer[%1]'                       # buffer exists
press 'i hello<esc>'
wait --ref '@vim.statusline[value~=/\+/]'          # modified marker
press ':w<cr>'
wait --ref '@vim.statusline[value~=/written/]'     # save echoed
snapshot --select '@vim.statusline'                # just the statusline
press ':q<cr>'
```

**Friction:** roughly equal. The selector form is slightly more
verbose but communicates *what you're waiting on* — "the modified
marker on the statusline" — instead of "this regex somewhere on
screen." That's a real readability win for agents that don't have
context on what `\[\+\]` means.

**Score:** ✅ Cleaner.

### 7.2 vim search (`vim_search`)

**Today:**
```bash
press "/<cr>"
type "needle"
press "<cr>"
wait --text "needle"
snapshot --mode adapter            # cmdline shows /needle
```

**With addressing model:**
```bash
press '/'
wait --ref '@vim.cmdline[focused]'                 # search prompt up
type 'needle'
press '<cr>'
wait --ref '@vim.cmdline[focused] --gone'          # prompt closed
snapshot --select '@vim.buffer'                    # what landed
```

**Friction:** the new `wait --gone` is doing real work here. Today
you wait for `"needle"` to appear, which can fire on the literal
*typed* text in the cmdline *before* the search actually executes —
a classic flake source. Waiting for the cmdline to close is the
correct event.

**Score:** ✅ Cleaner, less flaky.

### 7.3 htop F-key bar (`htop_bwrap.rs`)

**Today:**
```bash
spawn -- htop -d 50 -C --no-mouse
wait --text "F10"
snapshot
# assert outline contains "F10Quit" and "F1Help"
```

**With addressing model — needs an htop adapter:**
```bash
spawn -- htop -d 50 -C --no-mouse
wait --ref '@htop.fkey[10]'
snapshot --select '@htop.fkey'                     # all F-key labels
```

**Friction:** the existing test passes today *without* an htop
adapter — it works against the generic outline. The new form
*requires* an htop adapter to exist. If we ship the addressing
model without an htop adapter, existing tests still work
(generic outline + name regex), but the new idioms aren't usable
here.

**Score:** ⚠️ Neutral. The new idiom is only nicer once adapters
exist. Most current built-in adapters (vim, claude-code, shell)
will need a pass to emit hierarchical refs. List: vim, shell,
generic, claude-code → all need scheme + durable bindings.

### 7.4 fzf typed filter (`fzf_bwrap.rs` / intent skill)

**Today:**
```bash
spawn -- bash -c "echo -e 'apple\nbanana\ncherry' | fzf"
wait --text "3/3"
type "ban"
wait --text "1/3"
snapshot
press "<c-c>"
```

**With addressing model:**
```bash
spawn -- bash -c "echo -e 'apple\nbanana\ncherry' | fzf"
wait --ref '@fzf.matchcount[value=3]'
type 'ban'
wait --ref '@fzf.matchcount[value=1]'
snapshot --select '@fzf.candidates'
press '<c-c>'
```

**Friction:** clean if fzf adapter exists, otherwise no change.
Note: `[value=3]` predicate matches the structured count field,
not the rendered "3/3" string. More agent-friendly because the
agent doesn't need to know fzf's display format.

**Score:** ✅ Cleaner (adapter-gated).

### 7.5 mcp_drives_vim_bwrap (the headline E2E)

This is the test that asserts MCP → daemon → bwrap → vim composes.
JSON-RPC calls translate to CLI calls. The new selectors and `--to`
flow through MCP tool descriptions per §2.6.

**Today** (MCP `wait` tool):
```json
{"name":"wait","arguments":{"text":"sample.txt","max":5000}}
```

**With addressing model:**
```json
{"name":"wait","arguments":{"ref":"@vim.buffer[%1]","max":5000}}
{"name":"press","arguments":{"keys":"iX<esc>","to":"@vim.buffer[%1]"}}
```

**Friction:** new tool params (`ref`, `to`, `select`, `gone`) need
documentation in the MCP tool description. The agent (Claude / GPT)
parsing the tool description needs to grok the selector syntax.
This is where the **skills system** earns its keep — a
selector-cheatsheet skill explaining the grammar in one page.

**Score:** ✅ Cleaner once skill docs catch up. Without docs,
agents will guess selectors and waste turns.

### 7.6 opencode / pi fake-inference (`ask` recipes)

These are Mode A (`run --stdin`) — no PTY interaction, no
outline. The addressing model doesn't touch them. Recipes in
`recipes/ask/` may add a `[routing]` block (§2.3.1) but for Mode A
the routing fast-paths to "write stdin, capture stdout."

**Score:** ➖ No change. Correctly out of scope.

### 7.7 shell + OSC 133 (`shell_osc133.rs`)

**Today:**
```bash
spawn -- bash
wait --idle 100                                    # let prompt land
snapshot                                           # state = "shell"
```

**With addressing model:**
```bash
spawn -- bash
wait --ref '@shell.prompt[focused]'                # prompt is ready
snapshot --select '@shell'
```

**Friction:** the shell adapter today reports state via `PaneState`
(based on OSC 133). It does NOT emit a `@shell.prompt` ref. To use
the new idiom, the shell adapter needs to grow ref output. Worth
it — `wait --ref '@shell.prompt'` is precisely "the shell is ready
for input," which is what we mean.

**Score:** ✅ Cleaner once shell adapter emits refs.

### 7.8 The recursive case — driving claude-code via agent-tui

(Not an existing test; included to stress the model on its own
dogfood scenario.)

**Goal:** agent-tui runs claude-code in a pane, the outer agent
wants to send a prompt and wait for the response to stop streaming.

**With addressing model:**
```bash
spawn -- claude
wait --ref '@claude.input[focused]'
type 'explain this codebase'
press '<cr>'
wait --ref '@claude.response[name~=/done|ready|finished/i]'
snapshot --select '@claude.response'
```

**Friction:** the v2 Claude Code manifest gives screen-level prompt
and response refs, but it does not know provider-level finality. The
`[streaming=false]` predicate doesn't exist in §2.2, and a regex on
rendered text is only a fallback. Rich finality needs provider events
or a provider-specific adapter that can emit `role=response-final`.

**Score:** ⚠️ Exposes a grammar gap. Captured in §4.

### 7.9 The other recursive case — tmux nesting agent-tui

Out of scope for v1 — adapter composition is the v2 deliverable
(§2.4). Calling out so we don't pretend otherwise.

---

### Friction summary

| Scenario               | New form clearer? | Requires adapter work? |
|------------------------|-------------------|------------------------|
| vim open/edit/save     | ✅                | Yes (vim)              |
| vim search             | ✅✅              | Yes (vim)              |
| htop F-keys            | ➖ neutral        | Yes (htop, new)        |
| fzf typed filter       | ✅                | Yes (fzf, new)         |
| MCP-drives-vim         | ✅                | Plus skill docs        |
| opencode/pi (Mode A)   | ➖ N/A            | No                     |
| shell + OSC 133        | ✅                | Yes (shell)            |
| claude-code recursive  | ✅ (exposes gap)  | Yes (claude-code)      |
| tmux nesting           | n/a (v2)          | n/a                    |

**Where friction lands:** the addressing model is only as good as
the adapter ecosystem. v1 ships with vim + shell + claude-code +
generic adapters returning durable refs; htop and fzf are
nice-to-have follow-ups. Without adapter coverage the new idioms
silently degrade to "match nothing, fall back to text."

### Test migration plan

No backwards compat required (per user direction). For each test
file, rewrite the assertions in the new form:

- `vim_basic.rs` — replace 4 of 4 tests with `wait --ref` /
  `--select` forms. Use `@vim.buffer[%1]`, `@vim.statusline`,
  `@vim.cmdline`.
- `vim_bwrap.rs` — mirror the docker rewrites.
- `htop_bwrap.rs` — keep as-is until htop adapter ships; mark
  with a `// TODO(htop-adapter)` comment.
- `fzf_bwrap.rs` — keep until fzf adapter ships.
- `shell_osc133*.rs` — rewrite once shell adapter emits refs.
- `mcp_drives_vim_bwrap.rs` — rewrite using MCP `ref`/`to`/`select`
  parameters once those land.
- `vimtutor_*.rs` — rewrite using `@vim.buffer` for content
  assertions, `@vim.cmdline` for command-mode steps.

Net: ~12 test files touched, ~3 adapter rewrites (vim, shell,
claude-code), ~2 new adapters (htop, fzf) optional but nice. The
test churn proves the model in practice and immediately surfaces
adapter gaps.

## 8. Skill / docs impact

The current `core` skill explicitly says (line 68):

> Refs (`@e1`, `@e2`, …) are assigned fresh on every snapshot and go
> stale the moment the pane changes. Always re-snapshot before the
> next ref-based interaction.

This guidance is **wrong under the new model** — durable refs are
the whole point. Every skill page needs review.

**Skills requiring rewrite:**

- `core` — refs section is the load-bearing change. Replace the
  "always re-snapshot" guidance with "Durable refs survive frames;
  Generic refs don't — check the `binding.kind`." Add a selector
  cheatsheet (one screen of `[role=X]`, `[name~=/Y/]`, `>`, etc.).
- `vim` — promote `@vim.buffer` / `@vim.statusline` /
  `@vim.cmdline` to first-class examples. Replace the `--text`
  regex examples that read like ".*\\[\\+\\].*".
- `tui-apps` — likely the new home for the selector quick reference
  and the `--to` driving pattern, since both are most useful for
  multi-pane TUIs.
- `intent` — `ask` (Mode A) is unchanged; `edit` / `watch` may
  grow `--to` for "edit inside pane N of tmux" once composition
  lands.
- `shell` — once shell adapter emits `@shell.prompt`, the OSC 133
  state guidance becomes "watch the ref, not the state enum."
- `ai-cli` — Claude/Codex/Pi/Aider/OpenCode examples use provider refs
  such as `@claude.response`, `@codex.approval`, and `@pi.input`.
  Unknown agent CLIs fall back to `@generic` until they get a v2
  manifest or a richer provider adapter.

**New skill (proposed):** `addressing` — single-page reference for
the ref grammar, selector syntax, and `--to` routing. Linked from
`core`. ~150 lines.

**Conceptual messaging to agents:** the unifying metaphor is *the
outline is a DOM-lite tree; selectors are CSS-lite; refs are
stable IDs when the adapter can provide them*. Drop "@eN refs go
stale" from the agent's mental model entirely. The new mental
model is:

```
spawn → adapter attaches → outline has stable refs →
    wait/snapshot/press all take selectors or refs →
    the outline IS the addressable surface
```

If the agent understands DOM, they understand this. If not, they
understand by analogy after one example.

## 9. Iteration plan

1. Land selector parser + library tests (no CLI wiring yet). 2 days.
2. Land `Adapter::route` default impl + plumb `--to` through the CLI
   end-to-end with the generic adapter. 2 days.
3. Write the tmux adapter, just enough to emit `@tmux.pane[N]` from
   border parsing. 3 days.
4. Add tmux modal-overlay detection. 1-2 days.
5. Integration fixtures + bwrap parity. 2 days.

Total ~2 weeks. Each step is independently mergeable.
