# Fixture image: tig (terminal git log viewer) + a seeded git repo at
# /fixtures/repo with two commits — same shape as the lazygit fixture
# but a different TUI.
#
# Reuse note: the seed pattern (deterministic dates / author / commit
# hashes) is identical to the lazygit fixture. We don't share a base
# image because the apt install list differs and we want the rootfs
# tarball self-contained.

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        git \
        less \
        python3 \
        tig \
    && rm -rf /var/lib/apt/lists/*

ENV GIT_CONFIG_GLOBAL=/etc/gitconfig-fixture \
    GIT_CONFIG_SYSTEM=/dev/null \
    GIT_AUTHOR_NAME=Fixture \
    GIT_AUTHOR_EMAIL=fix@example.com \
    GIT_COMMITTER_NAME=Fixture \
    GIT_COMMITTER_EMAIL=fix@example.com \
    GIT_AUTHOR_DATE='2024-01-01T00:00:00Z' \
    GIT_COMMITTER_DATE='2024-01-01T00:00:00Z'

RUN printf '[init]\n\tdefaultBranch = main\n[user]\n\tname = Fixture\n\temail = fix@example.com\n[commit]\n\tgpgsign = false\n' \
        > /etc/gitconfig-fixture

# Two stable, deterministic commits — pinned dates produce stable SHAs.
RUN mkdir -p /fixtures/repo \
    && cd /fixtures/repo \
    && git init -q -b main \
    && printf 'one\n' > a.txt \
    && git add a.txt \
    && git commit -q -m 'add a.txt' \
    && printf 'two\n' > b.txt \
    && git add b.txt \
    && git commit -q -m 'add b.txt'

# Pinned tig config — disables every nondeterministic flourish.
# Several legacy `show-*` knobs (show-rev-graph, show-id, show-date)
# were removed in tig 2.5 in favor of per-column settings; including
# them produces a warning banner that wrecks the alt-screen render.
RUN printf '%s\n' \
        'set vertical-split = no' \
        'set mouse = no' \
        'set refresh-mode = auto' \
        'set line-graphics = ascii' \
        > /etc/tigrc-fixture

ENV TERM=xterm-256color \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    NO_COLOR=1 \
    TIGRC_USER=/etc/tigrc-fixture \
    TIGRC_SYSTEM=/dev/null \
    HOME=/root

WORKDIR /fixtures/repo

CMD ["sleep", "infinity"]
