# Fixture image: `less` pager + a deterministic 200-line text file
# at /fixtures/lorem.txt that scenarios search through.
#
# Determinism levers:
#  - `less -M` for the long-prompt status line (filename + line range
#    + percentage) so tests can `wait_text` for it
#  - LESS env is forced to `-M -X` so the binary's user-config dotfile
#    can't change behavior. `-X` disables the alt-screen save/restore,
#    which simplifies the post-quit assertion (`die` and we're back to
#    bash with no terminal mode switch to handle).

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        less \
    && rm -rf /var/lib/apt/lists/*

# Deterministic content: 200 numbered lines including known anchor
# strings the search scenarios target.
RUN mkdir -p /fixtures \
    && for i in $(seq 1 200); do \
         if [ "$i" = "42" ]; then \
           echo "line $i: the-answer-marker"; \
         elif [ "$i" = "150" ]; then \
           echo "line $i: deep-in-the-file-marker"; \
         else \
           echo "line $i: lorem ipsum dolor sit amet"; \
         fi; \
       done > /fixtures/lorem.txt

ENV TERM=xterm-256color \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    LESS=-M

CMD ["sleep", "infinity"]
