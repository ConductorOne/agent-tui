#!/usr/bin/env bash
# Watch a multi-step task with live progress.
# Proves: `agent-tui watch` streams chunks as they arrive, not at exit.
#
# The eval here is timing: each step prints with ~500ms between them,
# matching the producer's cadence (no buffering to the end).
#
# Usage: ./scripts/watch-build.sh

set -u
AT="${AT:-./target/debug/agent-tui}"
[ -x "$AT" ] || AT=agent-tui

# Producer: prints 5 steps with 500ms gaps.
PRODUCER='for i in 1 2 3 4 5; do
    echo "[step $i] working..."
    sleep 0.5
done
echo "build complete"'

# Pipe through `while read` to tag each line with a wall-clock timestamp
# from THIS shell — proves bytes arrived live.
START_EPOCH=$(date +%s%N)
"$AT" watch -- bash -c "$PRODUCER" | while IFS= read -r line; do
    NOW=$(date +%s%N)
    ELAPSED=$(( (NOW - START_EPOCH) / 1000000 ))
    printf "%4dms  %s\n" "$ELAPSED" "$line"
done
