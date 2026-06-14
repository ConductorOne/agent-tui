# syntax=docker/dockerfile:1
#
# Binary-only multi-arch image for agent-tui.
#
# Published to ghcr.io/conductorone/agent-tui by .github/workflows/ghcr.yml as a
# single multi-arch (linux/amd64 + linux/arm64) manifest. The image exists so
# downstream consumers can bake the binary out of it via
#   COPY --from=ghcr.io/conductorone/agent-tui /usr/local/bin/agent-tui ...
# so the runtime path `/usr/local/bin/agent-tui` is a hard contract.

# --- build stage --------------------------------------------------------------
# buildx compiles this once per target platform (under QEMU emulation for the
# non-native arch). agent-tui is a lean Rust binary — no BoringSSL / heavy
# native deps — so emulated arm64 compilation is acceptable (the KISS path, vs.
# a split-native-runner + digest-merge). `bookworm` keeps the build glibc in
# step with the distroless/cc-debian12 runtime below.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
# `--locked` builds against the committed Cargo.lock for reproducibility.
RUN cargo build --release --locked --bin agent-tui \
    && cp target/release/agent-tui /agent-tui

# --- runtime stage ------------------------------------------------------------
# distroless/cc supplies glibc + libgcc/libstdc++ that the (glibc) agent-tui
# binary links against (portable-pty needs libc, so `scratch` is not safe here).
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /agent-tui /usr/local/bin/agent-tui
# Binary-only image; consumers extract the binary. The entrypoint is just a
# sanity convenience (`docker run … --help`).
ENTRYPOINT ["/usr/local/bin/agent-tui"]
