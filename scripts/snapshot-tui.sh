#!/usr/bin/env bash
# Drive an interactive TUI (less), navigate it, extract text via
# snapshot. Proves: Mode-B interactive driving works end-to-end —
# spawn + wait + press + snapshot --mode text.
#
# Usage: ./scripts/snapshot-tui.sh

set -u
AT="${AT:-./target/debug/agent-tui}"
[ -x "$AT" ] || AT=agent-tui

# Prepare a file with anchored markers so we can verify navigation.
TMP=$(mktemp -t snapshot-tui.XXXXXX)
trap 'rm -f "$TMP"; "$AT" --session snapshot-tui-$$ die >/dev/null 2>&1' EXIT
{
    for i in $(seq 1 50); do echo "filler line $i"; done
    echo "=== ANCHOR ALPHA ==="
    for i in $(seq 1 5); do echo "post-alpha $i"; done
    echo "=== ANCHOR OMEGA ==="
    for i in $(seq 1 50); do echo "more filler $i"; done
} > "$TMP"

SESSION="snapshot-tui-$$"

# Spawn less. less waits at the top of the file.
"$AT" --session "$SESSION" spawn -- less "$TMP" >/dev/null
"$AT" --session "$SESSION" wait --text ":" --max 3000 >/dev/null

echo "=== STEP 1: snapshot at top ==="
"$AT" --session "$SESSION" --json snapshot --mode text \
    | python3 -c "import json,sys; print(json.load(sys.stdin)['data']['text'].split(chr(10))[0])"

echo "=== STEP 2: search for ALPHA marker ==="
# less search: /pattern then enter, then 'q' to cancel search highlight
"$AT" --session "$SESSION" send-ansi "$(printf '/ANCHOR ALPHA\r')" >/dev/null
"$AT" --session "$SESSION" wait --text "ANCHOR ALPHA" --max 3000 >/dev/null
"$AT" --session "$SESSION" --json snapshot --mode text \
    | python3 -c "
import json, sys
text = json.load(sys.stdin)['data']['text']
# Print just the lines containing 'ANCHOR' or 'post-alpha'
for ln in text.split(chr(10)):
    if 'ANCHOR' in ln or 'post-alpha' in ln:
        print('  ' + ln)"

echo "=== STEP 3: jump to end (G) ==="
"$AT" --session "$SESSION" press "G" >/dev/null
"$AT" --session "$SESSION" wait --text "(END)" --max 3000 >/dev/null
"$AT" --session "$SESSION" --json snapshot --mode text \
    | python3 -c "
import json, sys
text = json.load(sys.stdin)['data']['text']
lines = text.split(chr(10))
# Last 3 non-empty content lines
content = [l for l in lines if l.strip()]
print('  ' + content[-3])
print('  ' + content[-2])
print('  ' + content[-1])"

"$AT" --session "$SESSION" press "q" >/dev/null 2>&1
"$AT" --session "$SESSION" die >/dev/null 2>&1
