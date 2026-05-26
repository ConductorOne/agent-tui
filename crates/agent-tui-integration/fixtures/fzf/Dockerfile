# Fixture image: fzf (fuzzy picker) + a deterministic candidate list.
#
# Debian bookworm-slim ships fzf 0.38 in apt; we use that. Test
# scenarios pipe a known fruit list as stdin so the candidate set is
# stable across runs.

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        fzf \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /fixtures \
    && printf 'apple\napricot\nbanana\nblueberry\ncherry\ncoconut\ndate\nelderberry\nfig\ngrape\n' \
        > /fixtures/fruits.txt

ENV TERM=xterm-256color \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    HOME=/root \
    FZF_DEFAULT_OPTS=

CMD ["sleep", "infinity"]
