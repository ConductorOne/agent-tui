# Fixture image: GNU nano + a deterministic source file.
#
# Important determinism levers in test invocation:
#  - `-I` / --ignorercfiles — ignore /etc/nanorc + ~/.nanorc
#  - `-x` / --nohelp — drop the 2-row Ctrl-key help footer
#  - `-w` / --nowrap — disable auto hard-wrap
#  - `-R` / --restricted — sandboxed mode, no read-other-files
#  - NO_COLOR=1 — disable syntax highlighting
# Scenarios usually keep the help footer ON so they can assert against
# the literal `^X Exit` shortcut text.

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        nano \
    && rm -rf /var/lib/apt/lists/*

# Seed a starting file. The known-anchor "hello world from nano" string
# lets search scenarios find a needle in a small haystack.
RUN mkdir -p /fixtures \
    && printf 'hello world from nano\nsecond line\nthird line\n' \
        > /fixtures/sample.txt

ENV TERM=xterm-256color \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    NO_COLOR=1 \
    HOME=/root

CMD ["sleep", "infinity"]
