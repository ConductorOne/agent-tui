# agent-tui — common dev tasks.
# Install just from https://github.com/casey/just

set shell := ["bash", "-uc"]

# Default: pretty-print the recipe list.
default:
    @just --list

# Cargo check across the workspace (fast feedback loop).
check:
    cargo check --workspace --all-targets

# Build the workspace (debug).
build:
    cargo build --workspace --all-targets

# Build the release binary.
release:
    cargo build --release -p agent-tui

# Run the workspace test suite. Uses cargo-nextest if installed.
test:
    @if command -v cargo-nextest >/dev/null 2>&1; then \
        cargo nextest run --workspace; \
    else \
        cargo test --workspace; \
    fi

# Run clippy across the workspace with all targets.
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# Format check (CI-ready).
fmt-check:
    cargo fmt --all -- --check

# Apply formatting.
fmt:
    cargo fmt --all

# Run the daemon in the foreground (sees logs on stderr).
run-daemon SESSION="default":
    cargo run -p agent-tui -- --session {{SESSION}} daemon run

# Run any CLI subcommand against a (lazily-spawned) daemon.
# Example: `just run -- snapshot --json`
run *ARGS:
    cargo run -p agent-tui -- {{ARGS}}

# Walk the workspace looking for TODOs and unfinished scaffolding.
todos:
    @rg "TODO|FIXME|XXX|unimplemented!|todo!\(" --type rust || echo "no todos found"

# CI-equivalent: fmt + clippy + test.
ci: fmt-check clippy test
