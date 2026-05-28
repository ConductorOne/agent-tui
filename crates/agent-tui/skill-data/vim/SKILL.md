---
name: vim
description: Open files, edit, save, search in vim/nvim with adapter-aware state
allowed-tools: Bash(agent-tui:*)
---

# vim / nvim

The vim adapter parses vim's statusline + cmdline into structured
nodes so the agent can read mode, filename, line/col, and modified
state without splitting strings.

Outline shape (every frame, durable refs):

```
@vim                    role=root
├── @vim.mode           role=mode      value=insert|normal|visual|search|command|replace|…
├── @vim.file           role=file      value="modified" when [+]
├── @vim.statusline     role=statusline
├── @vim.cmdline        role=cmdline   focused=true while on `:` or `/`
└── @vim.buffer         role=buffer    focused=true otherwise
```

For selector syntax + the `--ref` / `--select` / `--to` flow,
read `agent-tui skills get addressing` first.

## Read [core](../core/SKILL.md) first
<!-- tested-by: navigation -->

This skill assumes the core loop. It covers what the **vim adapter**
adds on top.

## Opening a file
<!-- tested-by: bwrap_vim_opens_file_and_shows_content -->

```bash {test=vim-open}
agent-tui spawn -- vim /work/notes.md
agent-tui wait --ref '@vim.buffer'          # vim has rendered
agent-tui snapshot --select '@vim'          # the full vim subtree
agent-tui press ":q!<cr>"
```

The pane adapter is auto-detected as `vim` once the alt-screen flips
on. Once detected, the outline always has the `@vim` root with the
five children listed above. Use `--select '@vim'` to scope a
snapshot to just the editor, or address children directly.

## Editing and saving
<!-- tested-by: bwrap_vim_edit_save_round_trip -->

```bash {test=vim-edit-save}
agent-tui spawn -- vim /work/notes.md
agent-tui wait --ref '@vim.buffer'
agent-tui press "i hello-from-agent-tui<esc>"
agent-tui wait --ref '@vim.file[value=modified]'   # modified flag set
agent-tui press ":w<cr>"
agent-tui wait --ref '@vim.statusline[value~=/written/]'  # save confirmed
agent-tui press ":q<cr>"
```

`<esc>` returns to normal mode. `@vim.mode` carries the current mode
name in its `value` field (e.g. `value=insert`) — predicate
`@vim.mode[value=insert]` is the right test.

## Searching
<!-- tested-by: bwrap_vim_search_finds_target -->

```bash {test=vim-search}
agent-tui spawn -- vim /fixtures/sample.txt
agent-tui wait --ref '@vim.buffer'
agent-tui press '/'                            # opens search prompt
agent-tui wait --ref '@vim.cmdline[focused]'   # cmdline is up
agent-tui type 'needle'
agent-tui press '<cr>'
agent-tui wait --ref '@vim.cmdline[focused]' --gone  # cmdline closed
agent-tui snapshot --select '@vim.buffer'      # what the search landed on
agent-tui press ":q!<cr>"
```

Waiting on `@vim.cmdline[focused] --gone` is more reliable than
`--text` matching: the typed `/needle` shows in the cmdline before
vim executes the search, which can false-fire a text-regex wait.

## Reading the mode

`@vim.mode.value` is one of: `normal`, `insert`, `visual`,
`visual-line`, `visual-block`, `command`, `search`, `replace`. Reach
for `wait --ref '@vim.mode[value=insert]'` instead of regex-matching
"-- INSERT --" in rendered text.

## Insert mode

```bash {test=vim-insert-outline}
agent-tui spawn -- vim /fixtures/sample.txt
agent-tui wait --ref '@vim.buffer'
agent-tui press 'i'
agent-tui wait --ref '@vim.mode[value=insert]'
agent-tui press "<esc>:q!<cr>"
```

## Command-line buffer

When you press `:` or `/`, `@vim.cmdline` becomes focused and its
`value` carries the in-progress command:

```bash {test=vim-cmdline}
agent-tui spawn -- vim /fixtures/sample.txt
agent-tui wait --ref '@vim.buffer'
agent-tui type ':set nu'
agent-tui wait --ref '@vim.cmdline[focused]'
agent-tui snapshot --select '@vim.cmdline'   # value: ":set nu"
agent-tui press "<esc>:q!<cr>"
```

## Quitting and the alt-screen
<!-- tested-by: vim_quit_releases_alt_screen -->

When vim exits, the alt-screen is released and the adapter
auto-redetects (likely `Shell` if vim was launched from a bash). The
`@vim` root disappears from the outline; the new outline will have
`@shell` at the root instead.

## Walkthrough of vimtutor lessons 1.1–1.3
<!-- tested-by: bwrap_vimtutor_walkthrough_lessons_1_1_to_1_3 -->

A longer scenario that exercises navigation + delete + undo across
three vimtutor lessons. Useful as a reference for multi-step vim
workflows. See `crates/agent-tui-integration/tests/vimtutor_bwrap.rs`
for the full script.
