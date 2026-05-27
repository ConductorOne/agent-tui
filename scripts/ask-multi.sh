#!/usr/bin/env bash
# Ask multiple AI CLIs the same question, print each answer.
# Proves: `agent-tui ask` recipes work across providers.
#
# Usage:
#   ./scripts/ask-multi.sh
#   ./scripts/ask-multi.sh "your prompt here"

set -u

PROMPT="${1:-what is the square root of 144}"
AT="${AT:-./target/debug/agent-tui}"
[ -x "$AT" ] || AT=agent-tui

# Detect which CLIs are installed; skip missing.
PROVIDERS=()
for p in claude codex pi; do
    if command -v "$p" >/dev/null 2>&1; then
        PROVIDERS+=("$p")
    fi
done

echo "Q: $PROMPT"
echo

# As of recipe-driven `ask`, no per-provider extraction is needed
# here — the bundled recipe for each provider knows how to pull the
# answer out (codex's session prelude, etc.). This script is just
# a fan-out + timing wrapper now.
for p in "${PROVIDERS[@]}"; do
    printf "[%-8s] " "$p"
    START=$(date +%s%N)
    ANSWER=$("$AT" ask "$p" "$PROMPT" 2>/dev/null | head -3 | tr '\n' ' ')
    END=$(date +%s%N)
    MS=$(( (END - START) / 1000000 ))
    printf "%s (%dms)\n" "$ANSWER" "$MS"
done
