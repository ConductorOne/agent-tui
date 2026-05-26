# Fixture image: bash with FinalTerm/OSC 133 shell integration baked in.
#
# Designed for the agent-tui integration suite — every interactive bash
# session in here emits ESC ] 133 ; A/C/D markers around prompts and
# commands, which the agent-tui daemon's classifier translates to
# PaneState::Shell / PaneState::Running.
#
# Built locally (see `just fixtures` or the integration CI job) and
# tagged `agent-tui-fixture-shell:dev`. The agent-tui binary itself is
# bind-mounted in at test time, not baked in — that keeps the image
# stable across binary changes and avoids a rebuild per cargo target.

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        procps \
    && rm -rf /var/lib/apt/lists/*

# Drop the OSC 133 integration into profile.d so any interactive bash
# session sources it via `bash --login -i`.
COPY osc133.bash /etc/profile.d/osc133.sh

# Predictable prompt so tests can wait for `agent-tui-fixture$ `.
ENV PS1='agent-tui-fixture$ '

# Default cmd is sleep so the container stays Running while tests
# `docker exec` agent-tui into it. Override via the scenario harness.
CMD ["sleep", "infinity"]
