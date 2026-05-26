# Fixture image: htop + a known long-lived process so the process list
# isn't empty.
#
# htop is a live-redrawing TUI: by default it repaints every 1.5s, which
# would never let `wait --idle` settle. Test scenarios launch with
# `htop -d 50` (5 second refresh) so we have a long enough quiet window
# to snapshot inside.

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        htop \
        procps \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /root/.config/htop \
    && touch /root/.config/htop/htoprc

ENV TERM=xterm-256color \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    HOME=/root

CMD ["sleep", "infinity"]
