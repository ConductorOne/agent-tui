---
name: shell
description: Drive bash/zsh shell sessions; detect prompt vs. running command via OSC 133
allowed-tools: Bash(agent-tui:*)
---

# Shell sessions

Driving an interactive shell from an agent is the most common
agent-tui use-case. The hard problem is "is the shell at a prompt or
running a command?" — `agent-tui` solves this via the OSC 133
shell-integration protocol when the shell emits it, and falls back to
a heuristic when it doesn't.

## Read [core](../core/SKILL.md) first
<!-- tested-by: navigation -->

This skill assumes you know the core loop (spawn / wait / snapshot /
interact). It only covers what's different for shells.

## State detection: prompt vs. running
<!-- tested-by: bwrap_bash_with_osc133_classifies_as_shell -->

```bash {test=shell-state}
agent-tui spawn -- bash -i
agent-tui wait --text '\$ '                # wait for prompt
agent-tui snapshot --mode adapter           # shows `state: Shell`
```

The shell adapter classifies the pane state by walking the screen and
the recorder event log for OSC 133 `A`/`B`/`C`/`D` markers:

| OSC 133 | Adapter state |
|---|---|
| `B` (prompt-start) | `Shell` |
| `C` (command-start) | `Running` |
| `D` (command-end) | `Shell` (back to prompt) |

Without OSC 133, the adapter falls back to "if the last line looks
like a prompt regex, we're at a prompt; otherwise Unknown." The
state is exposed in `snapshot --mode adapter` and used by
`wait --state Shell` / `wait --state Running` (P2).

## Running a command and reading its output
<!-- tested-by: bwrap_bash_returns_to_shell_after_command_finishes -->

```bash {test=shell-run-cmd}
agent-tui spawn -- bash -i
agent-tui wait --text '\$ '
agent-tui type "ls /tmp"
agent-tui press "<cr>"
agent-tui wait --text '\$ '                # wait for the prompt again
agent-tui snapshot                          # capture output
```

The "wait for the prompt again" pattern is robust as long as the
prompt regex doesn't appear in the command's output. For commands
that might echo prompt-like text, use `wait --state Shell` (P2) or a
known sentinel.

## Running long-lived commands
<!-- tested-by: bwrap_bash_running_command_classifies_as_running -->

```bash {test=shell-long-running}
agent-tui spawn -- bash -i
agent-tui wait --text '\$ '
agent-tui type "sleep 5 && echo done"
agent-tui press "<cr>"
agent-tui snapshot --mode adapter           # state: Running
agent-tui wait --text "done"
agent-tui wait --text '\$ '                # wait for return to prompt
```

While the command runs, the adapter reports `state: Running`. The
recorder also emits a `Checkpoint` event every 1000 PTY mutations
so the trace shows progress for long outputs.

## Enabling OSC 133 in bash
<!-- tested-by: navigation -->

The bash fixture in our tests sets these vars before `bash -i`:

```bash
PROMPT_COMMAND='printf "\e]133;D;%s\e\\" "$?"; printf "\e]133;A\e\\"'
PS1='\[\e]133;B\e\\\]\u@\h \w \$ '
```

Source: `crates/agent-tui-integration/fixtures/bash-osc133.sh`. Real
shells with shell-integration (ITerm2, VS Code terminal, modern WezTerm)
already emit these by default.

## When NOT to use this skill
<!-- tested-by: navigation -->

- **vim/nvim inside a shell**: switch to [vim](../vim/SKILL.md). The
  adapter promotes from `Shell` → `Unknown` when the alt-screen
  toggles on, and vim's adapter takes over.
- **AI CLIs (opencode/pi/codex)**: those aren't shells —
  [ai-cli](../ai-cli/SKILL.md) covers the fake-inference + bwrap
  pattern.
