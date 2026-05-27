# Fixture image: Pi (earendil-works/pi) — a minimalist open-source TUI
# AI coding agent. Multi-provider (Anthropic / OpenAI / Google / Azure /
# Bedrock / Mistral / Groq) with an openai-compatible layer.
#
# Supply-chain hardening (mirrors fixtures/opencode/Dockerfile):
#  - Pinned to v0.75.5 (NOT `latest`).
#  - Release tarball verified by sha256 inside the build.
#  - Digests from the official GitHub releases API:
#       https://api.github.com/repos/earendil-works/pi/releases/tags/v0.75.5
#  - No `curl | sh` installer.

FROM debian:bookworm-slim

ARG PI_VERSION=0.75.5

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
         amd64) PI_ARCH=x64;   PI_SHA=ac687acd72705dabe967fd40d365317f68054878be1cb2c0ae9a0592fd108b5d ;; \
         arm64) PI_ARCH=arm64; PI_SHA=8b5c66fc3baee5fea4077f62307f792d6293660315b3ce4d0940c49353b45f50 ;; \
         *) echo "unsupported arch: $ARCH" >&2 ; exit 1 ;; \
       esac \
    && curl -fsSL -o /tmp/pi.tgz \
        "https://github.com/earendil-works/pi/releases/download/v${PI_VERSION}/pi-linux-${PI_ARCH}.tar.gz" \
    && echo "${PI_SHA}  /tmp/pi.tgz" | sha256sum -c - \
    # The Pi release tarball is a directory tree (`pi/` containing the
    # binary, node_modules/, assets/, README.md, etc.) — not a bare
    # binary. Extract to /opt/pi/ and symlink the binary into PATH.
    && mkdir -p /opt \
    && tar -xzf /tmp/pi.tgz -C /opt \
    && rm /tmp/pi.tgz \
    && chmod +x /opt/pi/pi \
    && ln -s /opt/pi/pi /usr/local/bin/pi

RUN mkdir -p /fixtures /work

ENV TERM=xterm-256color \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    HOME=/root

WORKDIR /work

CMD ["sleep", "infinity"]
