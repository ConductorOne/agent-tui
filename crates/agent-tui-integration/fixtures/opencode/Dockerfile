# Fixture image: OpenCode (sst/opencode) — an open-source TUI AI coding
# agent built on Bun + Ink.
#
# **STATUS: BUILT, NOT YET DRIVEN.** The image builds + verifies, and
# `opencode --version` runs inside the bwrap sandbox. End-to-end test
# scenarios are NOT in place because OpenCode uses the new OpenAI
# Responses API (`/v1/responses` with strict streaming event schema
# validation via the AI SDK's Zod schemas). The protocol surface is
# wider than the chat completions API our `fake_inference.rs` module
# currently emits, AND `opencode run` requires `--dangerously-skip-
# permissions` + closed stdin to complete a single non-interactive
# turn. See `tests/pi_fake_inference.rs` for the working AI-CLI loop —
# Pi uses `/v1/chat/completions` which is simpler and proven.
#
# To unblock OpenCode scenarios, the fake-inference module needs:
#  1. A `/v1/responses` handler that emits the full Responses event
#     stream (response.created → output_item.added → content_part.added
#     → output_text.delta×N → output_text.done → content_part.done →
#     output_item.done → response.completed) with valid sequence_number
#     ordering + matching item_id references.
#  2. Per-scenario `auth.json` or `--dangerously-skip-permissions`
#     wiring so opencode doesn't wait on TTY prompts for permission.
#
# **Supply-chain hardening notes** — this fixture is the canonical
# example of how to pin a third-party tool we don't control:
#  1. Pinned to v1.15.10 (NOT `latest`). Upgrading is an explicit
#     change to this Dockerfile.
#  2. The release tarball's sha256 is verified inside the image
#     build with `sha256sum -c -`. The digests came from the official
#     GitHub releases API:
#        https://api.github.com/repos/sst/opencode/releases/tags/v1.15.10
#     If the URL serves a different file, the build fails loudly.
#  3. No `curl | sh` installer pattern — download, verify, extract
#     each as separate steps.
#  4. `--no-install-recommends` on every apt-get to avoid pulling in
#     packages we didn't audit.

FROM debian:bookworm-slim

ARG OPENCODE_VERSION=1.15.10

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        curl \
        tar \
        gzip \
        ripgrep \
    && rm -rf /var/lib/apt/lists/* \
    && ARCH="$(dpkg --print-architecture)" \
    && case "$ARCH" in \
         amd64) OC_ARCH=x64;   OC_SHA=a4c0c94a7fdbf637e3ae479c046ca49e925370b4cee503dfba7ab677a13cd0c5 ;; \
         arm64) OC_ARCH=arm64; OC_SHA=def583a68cb155cddd9523e52139a5b915e981eef17a274c8c8171a3ee828ae2 ;; \
         *) echo "unsupported arch: $ARCH" >&2 ; exit 1 ;; \
       esac \
    && curl -fsSL -o /tmp/opencode.tgz \
        "https://github.com/sst/opencode/releases/download/v${OPENCODE_VERSION}/opencode-linux-${OC_ARCH}.tar.gz" \
    && echo "${OC_SHA}  /tmp/opencode.tgz" | sha256sum -c - \
    && tar -xzf /tmp/opencode.tgz -C /usr/local/bin \
    && rm /tmp/opencode.tgz \
    && chmod +x /usr/local/bin/opencode

# Pre-create the bind-mount points + a place for the per-scenario
# opencode.json config the scenario will drop.
RUN mkdir -p /fixtures /work /root/.config/opencode

# **Pre-seed OpenCode's bin cache with ripgrep.** OpenCode auto-
# downloads ripgrep on first run if it's not already at
# `<cache>/bin/rg`; that download is a hard hang/timeout under
# bwrap's network-restricted scenarios and unnecessary noise even
# when network is open. apt-get installed `ripgrep` above; symlink
# it into the path OpenCode expects. (Cache dir is per-user; HOME=/root.)
RUN mkdir -p /root/.cache/opencode/bin \
    && ln -s /usr/bin/rg /root/.cache/opencode/bin/rg

ENV TERM=xterm-256color \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    HOME=/root \
    XDG_CONFIG_HOME=/root/.config

WORKDIR /work

CMD ["sleep", "infinity"]
