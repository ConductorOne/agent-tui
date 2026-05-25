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
