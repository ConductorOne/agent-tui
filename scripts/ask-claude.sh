#!/usr/bin/env bash
# Run claude under agent-tui, type the prompt via the PTY, print the
# answer. The "subprocess as data" pattern in one line.
#
# Before this lived in this repo: ~89 lines of bash with a temp file,
# DONE marker, cat/tee wrapper, and trap-based cleanup. After
# `agent-tui run` landed: this file. 13 lines (including shebang +
# comments). The daemon owns spawn/stdin/wait/tail/die; the caller
# owns "what to ask."
#
# Usage:
#   ./scripts/ask-claude.sh
#   ./scripts/ask-claude.sh "your prompt here"

AT="${AT:-./target/debug/agent-tui}"
[ -x "$AT" ] || AT=agent-tui

exec "$AT" run --stdin "${1:-what is 40+2}" -- claude -p
