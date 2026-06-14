#!/usr/bin/env bash
# Bring up a Docker-API-compatible socket via rootful podman, suitable for
# pointing `DOCKER_HOST` at from the agent-tui integration test harness.
#
# Why this exists: in restricted / nested-container dev environments
# (corp Linux laptops with restrictive seccomp, etc.) rootless podman's
# user-namespace setup hits `newuidmap`/`uid_map` permission denials we
# can't undo without privileged pod flags. Rootful podman sidesteps the
# whole story; we just need to make the socket reachable from the dev
# user.
#
# Run once per session (or wire it into your shell rc). On environments
# where rootless podman *does* work (most laptops, GitHub Actions
# runners), prefer that — this script is a fallback, not the default.

set -euo pipefail

SOCKET_PATH="${SOCKET_PATH:-/run/podman/podman.sock}"
SOCKET_DIR="$(dirname "$SOCKET_PATH")"

# Bail early if it's already running.
if [[ -S "$SOCKET_PATH" ]] && curl -fs --unix-socket "$SOCKET_PATH" http://d/v1.41/_ping >/dev/null 2>&1; then
    echo "podman socket already up at $SOCKET_PATH" >&2
    echo "export DOCKER_HOST=unix://$SOCKET_PATH"
    exit 0
fi

# Need sudo for rootful podman + writable /run/podman/.
if ! sudo -n true 2>/dev/null; then
    echo "this script needs passwordless sudo; run \`sudo -v\` first" >&2
    exit 1
fi

sudo install -d -m 755 "$SOCKET_DIR"

# `podman system service --time=0` runs forever in the background. Use
# `nohup` so it survives this shell exiting; redirect logs so we can
# inspect if anything goes wrong.
LOG="${TMPDIR:-/tmp}/podman-service.log"
sudo nohup podman system service --time=0 "unix://$SOCKET_PATH" >"$LOG" 2>&1 &
disown || true

# Wait up to 5s for the socket to come up.
for _ in $(seq 1 50); do
    if [[ -S "$SOCKET_PATH" ]]; then
        sudo chmod 666 "$SOCKET_PATH"
        if curl -fs --unix-socket "$SOCKET_PATH" http://d/v1.41/_ping >/dev/null 2>&1; then
            echo "podman socket up at $SOCKET_PATH" >&2
            echo "export DOCKER_HOST=unix://$SOCKET_PATH"
            exit 0
        fi
    fi
    sleep 0.1
done

echo "timed out waiting for $SOCKET_PATH; see $LOG" >&2
exit 1
