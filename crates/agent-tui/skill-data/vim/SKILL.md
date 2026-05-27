---
name: vim
description: Open files, edit, save, search in vim/nvim with adapter-aware state
allowed-tools: Bash(agent-tui:*)
---

# vim / nvim

The vim adapter parses vim's statusline + cmdline into structured
nodes so the agent can read mode, filename, line/col, and modified
state without splitting strings.

## Read [core](../core/SKILL.md) first
<!-- tested-by: navigation -->

This skill assumes the core loop. It covers what the **vim adapter**
adds on top.

## Opening a file
<!-- tested-by: bwrap_vim_opens_file_and_shows_content -->

```bash {test=vim-open}
agent-tui spawn -- vim /work/notes.md
agent-tui wait --text "notes.md"
agent-tui snapshot --mode outline
agent-tui press ":q!<cr>"
```

The pane adapter is auto-detected as `vim` once the alt-screen flips
on. Subsequent snapshots include vim-specific structure (titlebar,
buffer, statusline, cmdline) instead of a flat character grid.

## Editing and saving
<!-- tested-by: bwrap_vim_edit_save_round_trip -->

```bash {test=vim-edit-save}
agent-tui spawn -- vim /work/notes.md
agent-tui wait --text "notes.md"
agent-tui press "i hello-from-agent-tui<esc>"
agent-tui wait --text "\[\+\]"              # vim's modified marker
agent-tui press ":w<cr>"
agent-tui wait --text "written"
agent-tui snapshot
agent-tui press ":q<cr>"
```

`<esc>` returns to normal mode. The adapter exposes the mode in the
adapter tree:

```bash
agent-tui snapshot --mode adapter           # `mode: Insert` etc.
```

## Searching
<!-- tested-by: bwrap_vim_search_finds_target -->

```bash {test=vim-search}
agent-tui spawn -- vim /fixtures/sample.txt
agent-tui wait --text "sample.txt"
agent-tui press "/<cr>"                     # opens search prompt
agent-tui type "needle"
agent-tui press "<cr>"
agent-tui wait --text "needle"
agent-tui snapshot --mode adapter           # cmdline shows "/needle"
agent-tui press ":q!<cr>"
```

## Adapter state on the modeline
<!-- tested-by: vim_modified_file_marks_status_node -->

The statusline node carries:

```
statusline:
  file: sample.txt
  modified: true
  mode: Insert
  line: 3
  col: 12
```

Asserting on these structured fields is much more reliable than regex
across rendered text. Use `--json` for machine consumption.

## Insert mode in the outline
<!-- tested-by: vim_insert_mode_shows_in_outline -->

The mode propagates to the outline:

```bash {test=vim-insert-outline}
agent-tui spawn -- vim /fixtures/sample.txt
agent-tui wait --text "sample.txt"
agent-tui press "i"
agent-tui wait --text "INSERT"
agent-tui snapshot --mode outline           # statusline node says "INSERT"
agent-tui press "<esc>:q!<cr>"
```

## Command-line buffer
<!-- tested-by: vim_command_mode_carries_command_line -->

When you press `:`, the cmdline node carries the in-progress
command:

```bash {test=vim-cmdline}
agent-tui spawn -- vim /fixtures/sample.txt
agent-tui wait --text "sample.txt"
agent-tui type ":set nu"
agent-tui snapshot --mode adapter           # cmdline: ":set nu"
agent-tui press "<esc>:q!<cr>"
```

## Quitting and the alt-screen
<!-- tested-by: vim_quit_releases_alt_screen -->

When vim exits, the alt-screen is released and the adapter
auto-redetects (likely `Shell` if vim was launched from a bash). The
ref space resets; treat the post-quit pane as a different pane.

## Walkthrough of vimtutor lessons 1.1–1.3
<!-- tested-by: bwrap_vimtutor_walkthrough_lessons_1_1_to_1_3 -->

A longer scenario that exercises navigation + delete + undo across
three vimtutor lessons. Useful as a reference for multi-step vim
workflows. See `crates/agent-tui-integration/tests/vimtutor_bwrap.rs`
for the full script.
