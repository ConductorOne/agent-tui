# Contributing to agent-tui

## Dev setup

```bash
# 1. Install rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 2. (optional) install just for the recipe shortcuts
cargo install just

# 3. Clone + build
git clone <this-repo>
cd agent-tui
just check    # fast — type-checks the workspace
just test     # runs the test suite
just build    # full debug build
```

`rust-toolchain.toml` pins `stable`; rustup will install the right components automatically.

## Where things live

- `crates/agent-tui` — the binary. CLI parsing + dispatch only.
- `crates/agent-tui-protocol` — wire types shared between CLI and daemon.
- `crates/agent-tui-engine` — the `Engine` trait the daemon talks to.
- `crates/agent-tui-engine-alacritty` — the default engine impl (`alacritty-terminal`-backed); the daemon instantiates this.
- `crates/agent-tui-engine-wezterm` — placeholder stub for a future `wezterm-term`-backed engine; not yet wired in.
- `crates/agent-tui-daemon` — long-lived daemon, socket server, per-pane logic.
- `crates/agent-tui-recorder` — asciicast-v3-extended event log.
- `crates/agent-tui-adapter` — per-program adapter trait + plug-in IPC.
- `docs/design/RFC.md` — the original architecture RFC (historical design note; may not match current behavior).

## Integration tests (Docker / Podman)

The real-world TUI suite lives in `crates/agent-tui-integration` and is
gated behind the `docker` Cargo feature. `cargo test --workspace` (i.e.
the default suite) skips it.

```bash
# Build the binary the harness will inject into containers.
cargo build --bin agent-tui

# Run the suite. Requires a Docker-API endpoint via DOCKER_HOST.
cargo test -p agent-tui-integration --features docker -- --nocapture
```

**Docker** works out of the box on `ubuntu-latest` and Docker Desktop.

**Podman** is supported transparently — testcontainers-rs talks to the
Docker HTTP API, which Podman exposes via `podman system service`. For
rootless dev environments:

```bash
podman system service --time=0 &
export DOCKER_HOST=unix:///run/user/$(id -u)/podman/podman.sock
cargo test -p agent-tui-integration --features docker
```

### Inside a restricted / nested-container dev env

The Docker backend **can't run nested in some restricted container
environments**: even rootful podman fails with `crun: mount proc to proc: Operation not
permitted` because the host pod's `/proc` is read-only and the runtime
can't manipulate the kernel namespaces it needs. Sysbox/Kata at the
node level would fix this; without that, the Docker path is CI-only.

For local iteration use the **bwrap backend** — same fixture
Dockerfiles, different sandbox runtime:

```bash
# One-shot: build the fixture images with podman, export each as a
# rootfs tarball into target/integration-rootfs/<name>/extracted/.
# ~1.5s per fixture; cached by Dockerfile-tree hash.
just rootfs

# Run the bwrap suite. ~5ms per scenario (vs Docker's ~3s container
# start) because there's no container daemon involved at test time.
just test-bwrap
```

`build-rootfs.sh` uses `sudo podman` for the build/export step
(rootful side-steps the newuidmap restriction). bwrap then sandbox-runs
each test as the dev user — no daemon, no socket, no nested-container
kernel features required at test time. It dodges the `mount proc` wall
by sharing the host's `/proc` read-only instead of remounting it.

CI runs **both** backends (`integration-docker` and `integration-bwrap`
jobs) on `ubuntu-latest` as a cross-check — both consume the same
Dockerfile fixtures, so a regression in either is a daemon-side bug
rather than runtime drift. See
`crates/agent-tui-integration/src/bwrap.rs` for the bwrap design and
`docs/research/testcontainers-spike.md` for the original Docker design.

On failure either harness writes diagnostic artifacts (command log,
snapshot history, asciicast, annotated PNG) to
`target/integration-artifacts/<test>/`. CI uploads them as
`integration-artifacts-docker` / `integration-artifacts-bwrap`.

## Pre-push checklist

CI runs the full matrix on every PR. To catch the common failures
**before** push, run these three commands locally:

```bash
cargo fmt --all --check                # CI's `fmt` job
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask cross-check                # macOS + Windows compile
```

The `cross-check` step is the one that catches platform-specific
breakage. It runs `cargo check --target <triple>` for macOS and
Windows from your Linux host (no full cross-toolchain needed — just
the target's stdlib via `rustup target add`). Skip via `--target
<custom>` to limit to a specific platform.

CI also runs `cargo xtask docs-coverage` + `cargo xtask
cli-coverage`. Both are fast — useful to include in the local loop:

```bash
cargo xtask docs-coverage              # every skill heading has a tested-by:
cargo xtask cli-coverage               # every CLI flag is documented
```

## Coding conventions

- `#![forbid(unsafe_code)]` in every crate root. If you need `unsafe`, justify it inline and downgrade to `#![deny(unsafe_code)]` for the file only.
- Errors: `thiserror` in libraries, `anyhow` in the binary.
- Logging: `tracing`. No `println!` in libraries.
- Tests live alongside the code in `#[cfg(test)] mod tests`. Integration tests go under `tests/`.
- Clippy is enforced in CI at `-D warnings`. Pre-commit: `just ci`.

## Design notes

`docs/design/` holds the original design RFCs. They predate the public release
and document the intended architecture; they may not match current behavior.
Treat the code as the source of truth.

## License

Apache-2.0. By contributing you agree your contribution is licensed under the project's Apache-2.0 license.
