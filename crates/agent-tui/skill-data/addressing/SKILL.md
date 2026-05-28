---
name: addressing
description: Selectors, refs, and targeted writes — the DOM-lite addressing model for nodes inside a pane
allowed-tools: Bash(agent-tui:*)
---

# Addressing — refs, selectors, routing

`agent-tui` exposes a pane's content as a tree of **outline nodes**.
Each node has a stable **ref** (like a DOM `id`), a **role** (like a
DOM tag), and child nodes. You address nodes with **selectors** (a
CSS-subset) and route writes with `press --to <selector>`.

If you know CSS and the DOM, you already know this skill.

## The mental model

```
outline (tree)
└── @vim                  role=root
    ├── @vim.mode         role=mode      value=insert
    ├── @vim.statusline   role=statusline
    ├── @vim.cmdline      role=cmdline   focused=true (during :  /)
    └── @vim.buffer       role=buffer    focused=true (otherwise)
```

- **Refs** like `@vim.buffer` are **durable**: same ref every frame
  for as long as the node exists. Old `@e1`/`@e2`/… per-snapshot
  refs are gone.
- **Selectors** are how you find nodes:
  `[role=cmdline][focused]`, `@vim.statusline`, `@tmux pane[%2]`.
- **Routing** is how you write into nested layouts:
  `press --to '@tmux.pane[%2]' 'iX<esc>'` lets the adapter translate
  into the actual byte sequence (tmux prefix-q-2, then the keys).

## Selectors at a glance

| You want | Selector |
|---|---|
| The focused buffer, wherever it is | `[role=buffer][focused]` |
| Vim's statusline | `@vim.statusline` |
| Vim's cmdline only while it's focused | `@vim.cmdline[focused]` |
| Any status node whose name contains "written" | `[role=status][name~=/written/]` |
| The second tmux pane (by stable id) | `@tmux.pane[%2]` |
| A buffer inside that pane | `@tmux.pane[%2] > [role=buffer]` |
| Only durable-bound refs (skip transient nodes) | `[durable]` |

Combinators:

- **Whitespace** = descendant: `@tmux [role=buffer]` matches any
  buffer anywhere below `@tmux`.
- **`>`** = direct child: `@tmux > [role=pane]` matches a pane that
  is an immediate child of `@tmux`.

Predicate operators (inside `[…]`):

- `attr=value` — exact (`role=buffer`)
- `attr~=/regex/` — Rust regex (`name~=/written/`)
- `attr^=prefix` — prefix (`name^=/work`)
- `attr$=suffix` — suffix (`name$=.txt`)

Attributes: `role`, `name`, `value`. Plus `[focused]` /
`[focused=false]` and `[durable]`.

## Using selectors

### `wait --ref` — block until a node matches

```bash {test=wait-ref-focused-cmdline}
agent-tui spawn -- vim /work/notes.md
agent-tui wait --ref '@vim.buffer'              # vim has rendered
agent-tui press '/'                             # open search
agent-tui wait --ref '@vim.cmdline[focused]'    # cmdline is up
agent-tui type 'needle'
agent-tui press '<cr>'
agent-tui wait --ref '@vim.cmdline[focused]' --gone   # cmdline closed
```

`--gone` inverts the match: fires when the selector matches **no**
node. Right answer for "wait for the confirm to dismiss" or "wait
for the search prompt to close."

Prefer `--ref` over `--text` when there's a structured node to
match — selectors don't false-fire on typed-but-not-yet-executed
text the way `--text` regex can.

### `snapshot --select` — return only matching nodes

```bash {test=snapshot-select}
agent-tui snapshot --select '@vim.statusline'
# → outline with just the statusline node + its anchor
agent-tui snapshot --select '[role=status]' --all
# → every status node in DFS pre-order
```

`--select` forces an outline even if you asked for `--mode text` or
`--mode cells`, so the response always carries something to interpret.

### `press --to` / `type --to` — route writes to a sub-node

Without `--to`, keys go straight to the pane's PTY (the classic
behavior). With `--to`, the pane's attached adapter translates
`(target, keys)` into a sequence of bytes — useful for multiplexers
like tmux that need a `<prefix>` dance before sending the actual key.

```bash
# Routed (adapter-specific, when supported):
agent-tui press --to '@tmux.pane[%2]' 'iX<esc>'

# Identity routing (when the adapter doesn't override) just sends
# the keys to the PTY — exactly like `press` without `--to`:
agent-tui press --to '[role=buffer][focused]' ':w<cr>'
```

If the selector doesn't match any node, you get `RoutingUnsupported`
(error code 2004) rather than a silent write. Take the hint: run
`snapshot --select '<your selector>'` to inspect.

## Ref grammar (when you need to write your own)

```
ref       = "@" head ( "." segment )*
head      = ident                  ; adapter root: tmux, vim, shell, …
segment   = ident                  ; e.g. .buffer
          | ident "[" key "]"      ; e.g. .pane[%2]
key       = ident                  ; named: [main]
          | integer                ; positional (NOT stable across frames)
          | "%" integer            ; durable adapter-assigned id
```

**Use `[%N]` when you need the same logical object across frames.**
The adapter stamps `durable=true` on these. Bare `[N]` keys are
positional and may shuffle if the adapter rearranges things.

## When to use which surface

| Goal | Use |
|---|---|
| Wait for vim to finish opening | `wait --ref '@vim.buffer'` |
| Wait for a `:write`-saved status | `wait --ref '[role=statusline][name~=/written/]'` |
| Pull just the buffer text | `snapshot --select '@vim.buffer'` |
| Pull every match in the outline | `snapshot --select <sel> --all` |
| Write into a specific pane | `press --to '<sel>' '<keys>'` |
| Wait for a transient prompt to close | `wait --ref '<sel>' --gone` |

## Error responses

The daemon's error responses are designed to help you self-correct:

- **Parse error** (bad grammar): the error message includes a
  `^`-pointer at the offending byte:
  ```text
  selector parse error at byte 6: unknown attribute "bogus"
    [bogus=x]
          ^
  ```
- **No-match on `--to <selector>`**: `error.hint` lists up to 20
  available refs from the current outline — copy one and retry.
- **No-match on `snapshot --select`**: the response succeeds with
  empty `outline.nodes`, plus a `warning` with code
  `selector_no_match` whose message lists available refs.

Common loop: snapshot first with no `--select` to discover refs,
then refine. Or — when in doubt — run `snapshot --select '*'` to
see every node.

## Common mistakes

1. **Re-snapshotting before each ref use.** Old guidance said
   "refs go stale every snapshot." That's gone — durable refs are
   stable. Snapshot once, address many times. (Per-frame `@eN`
   refs no longer exist.)
2. **`wait --text` when `wait --ref` is right.** `--text` is regex
   over the rendered grid; it can match the typed prefix of a
   command before vim executes it. `--ref` doesn't false-fire.
3. **Writing without `--to` into nested layouts.** Without `--to`,
   keys go to the outer PTY. Inside tmux that means everything
   targets the tmux prefix logic, not the inner app.
4. **Forgetting `--gone`.** Waiting for a confirm to *appear* and
   waiting for it to *dismiss* are different selectors only by
   `--gone`.

## What still requires text/regex

The selector grammar covers most cases but not all:

- Free-form output that doesn't go through a per-app adapter.
  Generic outline has a single `buffer` node with everything
  inside — `wait --text 'regex'` is fine here.
- Cell-position-exact assertions (color, width). Reach for
  `--mode cells`.
- OSC 133 shell-integration markers. Today the shell adapter sets
  `@shell.prompt[focused]` based on alt-screen mode; richer
  prompt-vs-running distinctions ride on the OSC 133 parser.

## Full reference

```bash
agent-tui skills get addressing --full
```

See also: `docs/addressing-rfc.md` for the design rationale and the
formal grammar.
