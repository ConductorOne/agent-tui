# Fixture image: lazygit + a deterministically-seeded git repo.
#
# Built locally via `just fixtures` (Docker backend) or `just rootfs lazygit`
# (bwrap backend). The agent-tui binary itself is bind-mounted in at
# scenario start time.
#
# Determinism:
#  - lazygit pinned to v0.61.1 with verified sha256.
#  - All git config localized inside /etc/gitconfig-fixture so the host's
#    user.signingkey, hooks, commit.gpgsign etc. can't leak in.
#  - Author + committer dates pinned to 2024-01-01T00:00:00Z so commit
#    hashes are stable across builds.
#  - The lazygit config disables network auto-fetch, nerd fonts, icons,
#    startup popups, and the version check — all of which would
#    otherwise vary the rendered output between machines or break
#    hermeticity in NetworkMode=none scenarios.

FROM debian:bookworm-slim

ARG LAZYGIT_VERSION=0.61.1

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        curl \
        git \
        less \
        locales \
        ncurses-bin \
        tar \
    && rm -rf /var/lib/apt/lists/* \
    && sed -i 's/^# *\(C.UTF-8\)/\1/' /etc/locale.gen 2>/dev/null || true \
    && locale-gen C.UTF-8 2>/dev/null || true \
    && ARCH="$(dpkg --print-architecture)" \
    && case "$ARCH" in \
         amd64) LG_ARCH=x86_64; LG_SHA=1b91e660700f2332696726b635202576b543e2bc49b639830dccd26bc5160d5d ;; \
         arm64) LG_ARCH=arm64;  LG_SHA=20b1abb2bee5dfd46173b9047353eb678bc51a23839e821958d0b1863ab1655e ;; \
         *) echo "unsupported arch: $ARCH" >&2 ; exit 1 ;; \
       esac \
    && curl -fsSL -o /tmp/lg.tgz \
        "https://github.com/jesseduffield/lazygit/releases/download/v${LAZYGIT_VERSION}/lazygit_${LAZYGIT_VERSION}_linux_${LG_ARCH}.tar.gz" \
    && echo "${LG_SHA}  /tmp/lg.tgz" | sha256sum -c - \
    && tar -xzf /tmp/lg.tgz -C /usr/local/bin lazygit \
    && rm /tmp/lg.tgz \
    && chmod +x /usr/local/bin/lazygit

# Lock git config to a fixture-owned file. GIT_CONFIG_SYSTEM=/dev/null is
# the documented way to ignore /etc/gitconfig too.
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

# Seed /fixtures/repo with a predictable mix:
#  - two clean commits ("add a.txt", "add b.txt")
#  - b.txt then modified in the working tree     -> ` M b.txt`
#  - c.txt new and staged                          -> `A  c.txt`
#  - d.txt new and untracked                       -> `?? d.txt`
RUN mkdir -p /fixtures/repo \
    && cd /fixtures/repo \
    && git init -q -b main \
    && printf 'one\n' > a.txt \
    && git add a.txt \
    && git commit -q -m 'add a.txt' \
    && printf 'two\n' > b.txt \
    && git add b.txt \
    && git commit -q -m 'add b.txt' \
    && printf 'two\nedited\n' > b.txt \
    && printf 'three\n' > c.txt \
    && git add c.txt \
    && printf 'untracked\n' > d.txt

# Pinned lazygit config. Disables every nondeterministic feature that
# could vary the rendered output across machines (icons, nerd fonts,
# auto-fetch, version checks, popups).
COPY config.yml /fixtures/xdg/lazygit/config.yml

ENV HOME=/fixtures \
    XDG_CONFIG_HOME=/fixtures/xdg \
    TERM=xterm-256color \
    COLORTERM=truecolor \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8

WORKDIR /fixtures/repo

CMD ["sleep", "infinity"]
