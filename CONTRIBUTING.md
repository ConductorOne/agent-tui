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
- `crates/agent-tui-engine-wezterm` — default engine impl.
- `crates/agent-tui-engine-alacritty` — lean alternative engine impl.
- `crates/agent-tui-daemon` — long-lived daemon, socket server, per-pane logic.
- `crates/agent-tui-recorder` — asciicast-v3-extended event log.
- `crates/agent-tui-adapter` — per-program adapter trait + plug-in IPC.
- `docs/RFC.md` — the canonical architecture RFC. Read this first.

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

### Inside a nested-container dev env (e.g. Squire EKS pods)

Rootless podman often fails in these envs — `newuidmap` can't write
`/proc/<pid>/uid_map` because the host pod's security context blocks
nested user-namespace setup, even when `/etc/subuid` and the
`newuidmap`/`newgidmap` setuid bits look correct. There's no clean
rootless fix without changing the EKS node runtime (Sysbox / Kata would
solve it; `privileged: true` on the pod is the heavy-hammer alternative).

For everyday dev iteration there's a pragmatic shortcut: run a rootful
podman socket the dev user can connect to via `DOCKER_HOST`:

```bash
eval "$(./scripts/dev/podman-socket.sh)"
cargo build --bin agent-tui
cargo test -p agent-tui-integration --features docker
```

The script:
- creates `/run/podman/` if missing,
- starts `podman system service --time=0` as root (idempotent),
- `chmod 666`s the socket so the dev user can connect,
- prints the `export DOCKER_HOST=…` line for `eval`.

This is a dev-only convenience — CI (GitHub Actions) uses Docker
directly, so the production CI matrix doesn't depend on the script. The
upshot is that the integration suite *can* be iterated on locally even
inside Squire without anyone needing to weaken the pod's security
context.

On failure the harness writes diagnostic artifacts (command log, last
snapshot, daemon response history) to
`target/integration-artifacts/<test>/`. CI uploads that directory as a
`integration-artifacts` action artifact for downloadable inspection.

See `docs/research/testcontainers-spike.md` for the full plan.

## Coding conventions

- `#![forbid(unsafe_code)]` in every crate root. If you need `unsafe`, justify it inline and downgrade to `#![deny(unsafe_code)]` for the file only.
- Errors: `thiserror` in libraries, `anyhow` in the binary.
- Logging: `tracing`. No `println!` in libraries.
- Tests live alongside the code in `#[cfg(test)] mod tests`. Integration tests go under `tests/`.
- Clippy is enforced in CI at `-D warnings`. Pre-commit: `just ci`.

## Working on the RFC

`docs/RFC.md` is the source of truth for the design. If you change behavior in a way that drifts from the RFC, update the RFC in the same PR.

## License

Apache-2.0. By contributing you agree your contribution is licensed under the project's Apache-2.0 license.
