#!/usr/bin/env bash
# Open a file in an editor via agent-tui, make programmatic edits, show
# the diff. Uses a non-interactive editor wrapper (sed) so the script
# is deterministic — for real interactive use, the same `edit` verb
# spawns $EDITOR.
#
# Proves: `agent-tui edit` round-trips: pre-content → editor mutates →
# post-content captured.
#
# Usage: ./scripts/edit-and-diff.sh

set -u
AT="${AT:-./target/debug/agent-tui}"
[ -x "$AT" ] || AT=agent-tui

TMP=$(mktemp -t agent-tui-edit.XXXXXX)
trap 'rm -f "$TMP"' EXIT

cat > "$TMP" <<'EOF'
first line
second line
third line
EOF

echo "=== BEFORE ==="
cat "$TMP"

# A scripted "editor" that mutates the file in place. In real use:
#   ./scripts/edit-and-diff.sh   # uses $EDITOR (default vim)
# Here we override --editor to a non-interactive shell that does the
# edit so the demo is reproducible.
FAKE_EDITOR=$(mktemp -t fake-editor.XXXXXX)
chmod +x "$FAKE_EDITOR"
cat > "$FAKE_EDITOR" <<'EOF'
#!/usr/bin/env bash
# This is the "editor": replace "second line" with "SECOND LINE!".
sed -i 's/second line/SECOND LINE!/' "$1"
EOF

echo "=== EDITING via agent-tui edit --editor $FAKE_EDITOR ==="
"$AT" edit "$TMP" --editor "$FAKE_EDITOR" >/dev/null

echo "=== AFTER ==="
cat "$TMP"

echo "=== DIFF ==="
# Reconstruct the original to show the diff
cat > "$TMP.orig" <<'EOF'
first line
second line
third line
EOF
diff "$TMP.orig" "$TMP" || true
rm -f "$TMP.orig" "$FAKE_EDITOR"
