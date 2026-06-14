---
name: tui-apps
description: Drive common TUI apps — htop, less, lazygit, tig, fzf, nano — for engine-correctness coverage
allowed-tools: Bash(agent-tui:*)
---

# Common TUI apps

Recipes + tested fixtures for the most common terminal apps an agent
might want to drive. Each section maps to a real integration test in
`crates/agent-tui-integration/tests/`.

## Read [core](../core/SKILL.md) first
<!-- tested-by: navigation -->

This skill assumes the core loop and adds per-app recipes.

## htop — process viewer
<!-- tested-by: bwrap_htop_renders_process_list_and_fkeys -->

```bash {test=htop-basic}
agent-tui spawn -- htop --no-color
agent-tui wait --text "F10"                 # F-key footer is ready
agent-tui snapshot --mode outline
agent-tui press "q"
```

## htop — setup screen modal
<!-- tested-by: bwrap_htop_setup_screen_opens_and_closes -->

```bash {test=htop-setup-screen}
agent-tui spawn -- htop --no-color
agent-tui wait --text "F10"
agent-tui press "<f2>"                      # Setup
agent-tui wait --text "Setup"               # the Setup screen's title
agent-tui press "<esc>"
agent-tui wait --text "F10"
agent-tui press "q"
```

Modal transitions exercise the engine's alt-screen + escape-sequence
handling. Confirms refs reset cleanly after a modal close.

## htop — tree mode toggle
<!-- tested-by: bwrap_htop_tree_toggle_changes_fkey_label -->

```bash {test=htop-tree}
agent-tui spawn -- htop --no-color
agent-tui wait --text "F10"
agent-tui press "<f5>"                      # toggle tree mode
agent-tui wait --text "├─|\|--"             # tree glyphs — Unicode OR ascii fallback
agent-tui snapshot
agent-tui press "q"
```

## less — file viewer
<!-- tested-by: bwrap_less_opens_file_and_shows_status -->

```bash {test=less-open}
agent-tui spawn -- less /work/big-file.txt
agent-tui wait --text "big-file"            # less puts the FILENAME in its status line
agent-tui wait --idle 150                   # let the first screenful settle
agent-tui snapshot
agent-tui press "q"
```

> **less has no `:` prompt to wait on.** On open, `less` renders the
> first screenful and shows the **filename** in the status line (or
> `(END)` if the whole file fits) — it does *not* print a bare `:` until
> *you* issue a command. Wait on the filename (or any line you know is on
> screen) and/or `--idle` to settle; `wait --text ":"` will time out.

## less — slash search
<!-- tested-by: bwrap_less_search_finds_anchor -->

```bash {test=less-search}
agent-tui spawn -- less /work/big-file.txt
agent-tui wait --text "big-file"            # filename in the status line (not ":")
agent-tui send-ansi 2f6e6565646c650d        # bytes for /needle<CR>
agent-tui wait --text "needle"
agent-tui snapshot
agent-tui press "q"
```

## less — jump to end (`G`)
<!-- tested-by: bwrap_less_jump_to_end_shows_end_marker -->

```bash {test=less-end}
agent-tui spawn -- less /work/big-file.txt
agent-tui wait --text "big-file"            # filename in the status line (not ":")
agent-tui press "G"
agent-tui wait --text "(END)"
agent-tui snapshot
agent-tui press "q"
```

## fzf — fuzzy finder, candidate list
<!-- tested-by: bwrap_fzf_opens_with_candidate_list -->

```bash {test=fzf-open}
agent-tui spawn -- bash -c "echo -e 'apple\nbanana\ncherry' | fzf"
agent-tui wait --text "3/3"                 # all candidates visible
agent-tui snapshot
agent-tui press "<c-c>"                     # cancel
```

## fzf — typed filter narrows
<!-- tested-by: bwrap_fzf_typed_filter_narrows_candidates -->

```bash {test=fzf-filter}
agent-tui spawn -- bash -c "echo -e 'apple\nbanana\ncherry' | fzf"
agent-tui wait --text "3/3"
agent-tui type "ban"
agent-tui wait --text "1/3"                 # only banana matches
agent-tui snapshot
agent-tui press "<c-c>"
```

## fzf — selection goes to stdout
<!-- tested-by: bwrap_fzf_select_outputs_selection_to_stdout -->

```bash {test=fzf-select}
agent-tui spawn -- bash -c "echo -e 'a\nb\nc' | fzf > /work/picked.txt"
agent-tui wait --text "3/3"
agent-tui press "<cr>"                      # pick the first
agent-tui wait --idle 200
# Selection landed in /work/picked.txt.
```

## lazygit — git TUI
<!-- tested-by: bwrap_lazygit_renders_seeded_state -->

```bash {test=lazygit-render}
agent-tui spawn -- lazygit -p /work/repo
agent-tui wait --text "Files"
agent-tui snapshot
agent-tui press "q"
```

## lazygit — switching panels
<!-- tested-by: bwrap_lazygit_navigates_to_branches_panel -->

```bash {test=lazygit-branches}
agent-tui spawn -- lazygit -p /work/repo
agent-tui wait --text "Files"
agent-tui press "<tab>"                     # move panel focus
agent-tui press "<tab>"
agent-tui wait --text "Local branches"      # the Branches panel's title is "Local branches"
agent-tui snapshot
agent-tui press "q"
```

## lazygit — commits panel
<!-- tested-by: bwrap_lazygit_commits_panel_shows_seeded_history -->

```bash {test=lazygit-commits}
agent-tui spawn -- lazygit -p /work/repo
agent-tui wait --text "Files"
agent-tui press "<tab><tab><tab>"           # walk to Commits
agent-tui wait --text "Commits"
agent-tui snapshot                          # shows seeded commit log
agent-tui press "q"
```

## tig — git log viewer
<!-- tested-by: bwrap_tig_main_view_shows_commits -->

```bash {test=tig-main}
agent-tui spawn -- bash -c "cd /work/repo && tig"
agent-tui wait --text "\[main\]"            # tig labels the main view "[main]" in its title
agent-tui snapshot
agent-tui press "q"
```

## tig — enter a diff view
<!-- tested-by: bwrap_tig_enter_opens_diff_view -->

```bash {test=tig-diff}
agent-tui spawn -- bash -c "cd /work/repo && tig"
agent-tui wait --text "\[main\]"            # main view label
agent-tui press "<cr>"                      # open diff for selected commit
agent-tui wait --text "\[diff\]"            # tig's diff view label
agent-tui snapshot
agent-tui press "q"                         # back to main
agent-tui press "q"                         # exit
```

## tig — alt-screen release on quit
<!-- tested-by: bwrap_tig_quit_releases_alt_screen -->

After `q`, tig releases the alt-screen and the adapter re-detects
the parent shell. Useful smoke check for adapter-promotion logic
after a TUI app exits.

## nano — opens with default chrome
<!-- tested-by: bwrap_nano_opens_file_with_chrome -->

```bash {test=nano-open}
agent-tui spawn -- nano /work/notes.md
agent-tui wait --text "\\^X Exit"           # nano's bottom bar
agent-tui snapshot
agent-tui press "<c-x>"
```

## nano — typing marks the buffer modified
<!-- tested-by: bwrap_nano_typed_buffer_shows_modified -->

```bash {test=nano-typed}
agent-tui spawn -- nano /work/notes.md
agent-tui wait --text "\\^X Exit"
agent-tui type "hello"
agent-tui wait --text "Modified"            # nano's title bar
agent-tui snapshot
agent-tui press "<c-x>n"                    # discard
```

## nano — save clears modified
<!-- tested-by: bwrap_nano_save_clears_modified -->

```bash {test=nano-save}
agent-tui spawn -- nano /work/notes.md
agent-tui wait --text "\\^X Exit"
agent-tui type "hello"
agent-tui wait --text "Modified"
agent-tui press "<c-o><cr>"                 # save (Ctrl-O, confirm)
agent-tui wait --idle 200
agent-tui snapshot                          # Modified is gone
agent-tui press "<c-x>"
```
