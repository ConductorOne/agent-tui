# Fixture image: vim + a deterministic /fixtures dir for edit/save/search
# scenarios.
#
# Built locally (see `just fixtures`) and tagged `agent-tui-fixture-vim:dev`.
# The agent-tui binary itself is bind-mounted in at scenario start time.
#
# Includes the standard `vimtutor` distribution so the vimtutor demo
# scenario can drive the built-in lesson walk-through end-to-end.

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        vim \
        vim-runtime \
    && rm -rf /var/lib/apt/lists/*

# Seed deterministic input files.
RUN mkdir -p /fixtures \
    && printf 'first line\nsecond line\nthird line\n' > /fixtures/sample.txt \
    && printf 'foo one\nfoo two\nfoo three\n' > /fixtures/search-target.txt

# Vim config: no swapfile (avoids prompts), nocompatible, predictable
# statusline so scenarios can wait for it.
COPY vimrc /etc/vim/vimrc.local

# Keep the container Running for `docker exec`.
CMD ["sleep", "infinity"]
