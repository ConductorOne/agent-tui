#!/usr/bin/env bash
# Build OCI rootfs tarballs from each in-tree integration fixture.
#
# For every `crates/agent-tui-integration/fixtures/<name>/Dockerfile`:
#   1. podman build  (--isolation=chroot — needed in restricted dev envs
#                     where the default user-ns isolation can't remount
#                     /proc; chroot mode does the build without it)
#   2. podman create (without start — metadata-only, no kernel work)
#   3. podman export → target/integration-rootfs/<name>/rootfs.tar
#   4. tar -x        → target/integration-rootfs/<name>/extracted/
#
# Caches by Dockerfile-tree sha. Reruns rebuild only when the fixture
# source actually changed.
#
# Consumed by the bwrap-backed Scenario (`crates/agent-tui-integration/
# src/bwrap.rs`) which `--ro-bind`s the extracted dir into a sandbox.
#
# Usage:
#   scripts/dev/build-rootfs.sh             # all fixtures
#   scripts/dev/build-rootfs.sh vim         # one fixture
#   FORCE=1 scripts/dev/build-rootfs.sh vim # rebuild even if cache hits
#
# Requires `sudo podman` (rootful, because rootless can't write uid_map
# in this env — see scripts/dev/podman-socket.sh for the parallel
# story). `sudo` is only needed at fixture-build time; the extracted
# rootfs is chown'd back to the invoking user so bwrap can read it
# without privileges at test time.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FIXTURES_DIR="$REPO_ROOT/crates/agent-tui-integration/fixtures"
OUT_DIR="$REPO_ROOT/target/integration-rootfs"

PODMAN="${PODMAN:-sudo podman}"

fixture_hash() {
    # Hash the entire fixture dir so any file change busts the cache.
    local dir="$1"
    find "$dir" -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum | awk '{print $1}'
}

build_one() {
    local name="$1"
    local src="$FIXTURES_DIR/$name"
    local out="$OUT_DIR/$name"
    local tag="agent-tui-fixture-${name}:dev"

    if [[ ! -f "$src/Dockerfile" ]]; then
        echo "skip $name: no Dockerfile at $src" >&2
        return 0
    fi

    mkdir -p "$out"
    local hash
    hash="$(fixture_hash "$src")"
    local stamp="$out/.hash"
    local force="${FORCE:-}"

    if [[ -z "$force" && -f "$stamp" && -d "$out/extracted" && "$(cat "$stamp")" == "$hash" ]]; then
        echo "==> $name: up-to-date (hash $hash)"
        return 0
    fi

    echo "==> $name: building"
    $PODMAN build --isolation=chroot -t "$tag" "$src" >/dev/null

    echo "==> $name: exporting rootfs"
    local cid
    cid="$($PODMAN create "$tag" sleep infinity)"
    # shellcheck disable=SC2064
    trap "$PODMAN rm '$cid' >/dev/null 2>&1 || true" EXIT
    $PODMAN export "$cid" > "$out/rootfs.tar"
    $PODMAN rm "$cid" >/dev/null
    trap - EXIT

    echo "==> $name: extracting"
    rm -rf "$out/extracted"
    mkdir -p "$out/extracted"
    sudo tar -xf "$out/rootfs.tar" -C "$out/extracted"
    sudo chown -R "$(id -un):$(id -gn)" "$out/extracted"

    # Pre-create the mountpoint dirs bwrap binds onto. Without these,
    # `bwrap --ro-bind <rootfs> /` makes / read-only and the subsequent
    # `--bind <scratch> /work` fails because mkdir /work can't write
    # to a ro filesystem. The dirs are empty; bwrap mounts over them.
    mkdir -p "$out/extracted/work"

    echo "$hash" > "$stamp"
    local size
    size="$(du -sh "$out/extracted" | awk '{print $1}')"
    echo "==> $name: ready ($size at $out/extracted)"
}

main() {
    if (( $# == 0 )); then
        for d in "$FIXTURES_DIR"/*/; do
            build_one "$(basename "$d")"
        done
    else
        for name in "$@"; do
            build_one "$name"
        done
    fi
}

main "$@"
