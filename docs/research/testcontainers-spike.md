# Research spike: real-world TUI integration tests via testcontainers-rs

**Status:** Spike, not yet decided.
**Author:** generated for PR #1 discussion.
**Tracker:** add to `tracker.md` "Open questions" once we land on an approach.

## The hole this fills

Today's tests cover the agent-tui *machinery* — engine fidelity, wire
protocol, governance, OSC 133 parsing — but every PTY-driven test spawns
`/bin/sh`, `/bin/cat`, or `printf`. We have **zero coverage** of the actual
agent-facing user stories:

- "Press `i hello<esc>:w<cr>` in vim, then snapshot — did the file save?"
- "Spawn lazygit, press `j j SPACE`, snapshot — is the right line selected?"
- "Open k9s, navigate to a CrashLoop pod, press `:`, type `logs`, press
  `<cr>` — does the outline now contain log lines?"
- "Spawn claude-code (the Ink-banner one), wait for the input prompt,
  type a question, observe quiesce barrier behavior under Ink's repaint
  cadence."

The whole point of the v1 product is making these work. Right now we ship
22 commits of substrate without a single regression test that would catch
"vim's `:w` stopped working" before a customer noticed.

## The proposal

Use [`testcontainers-rs`](https://rust.testcontainers.org/) to spin up a
**hermetic Docker container per test scenario**, mount or inject the
just-built `agent-tui` binary, run a scripted sequence of CLI calls
against a real TUI inside the container, and assert on snapshot outputs.

When a scenario fails, dump the container logs, the asciicast cast file,
and the final snapshot grid as test artifacts so the failure is
debuggable from the CI log alone.

## What testcontainers-rs gives us

From the [quickstart](https://rust.testcontainers.org/quickstart/testcontainers/):

```rust
use testcontainers::{runners::AsyncRunner, GenericImage};

#[tokio::test]
async fn vim_edits_then_saves() -> anyhow::Result<()> {
    let container = GenericImage::new("ghcr.io/agent-tui/vim-fixture", "latest")
        .with_copy_to("/usr/local/bin/agent-tui", binary_path()?)
        .start()
        .await?;
    // exec agent-tui inside the container, snapshot, assert
    let output = container
        .exec(ExecCommand::new(["agent-tui", "spawn", "vim", "/tmp/file"]))
        .await?;
    // ...
}
```

Highlights:
- Async tokio integration — drops into our existing test harness.
- `with_copy_to` injects the host binary into the container without
  rebuilding the image.
- `exec` runs commands inside the container, returns stdout/stderr.
- Containers are torn down on `Drop` (with cleanup-on-panic semantics).
- Image construction is cached by digest — re-runs reuse layers.

## Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│  host: cargo test  (CI runner or dev box with Docker)            │
│  ┌──────────────────────────────────────────────────────────┐    │
│  │  test scenario: "vim_save_round_trip"                    │    │
│  │  ┌────────────────────────────────────────────────────┐  │    │
│  │  │  testcontainer image: ghcr.io/<org>/vim-fixture    │  │    │
│  │  │   - vim + nvim installed                           │  │    │
│  │  │   - test fixtures in /fixtures                     │  │    │
│  │  │   - injected: /usr/local/bin/agent-tui (host build)│  │    │
│  │  │                                                    │  │    │
│  │  │   $ agent-tui spawn vim /fixtures/sample.txt       │  │    │
│  │  │   $ agent-tui wait --text '"sample.txt"'           │  │    │
│  │  │   $ agent-tui press 'i hello<esc>:w<cr>'           │  │    │
│  │  │   $ agent-tui wait --text 'written'                │  │    │
│  │  │   $ agent-tui snapshot --mode outline (assert)     │  │    │
│  │  │   $ agent-tui die                                  │  │    │
│  │  └────────────────────────────────────────────────────┘  │    │
│  │  on failure: collect cast file + snapshot + container    │    │
│  │  logs → write to target/integration-artifacts/<test>/    │    │
│  └──────────────────────────────────────────────────────────┘    │
└──────────────────────────────────────────────────────────────────┘
```

## Scenario DSL

Two options for how scenarios are expressed:

### A. Plain Rust (no DSL)

```rust
#[testcontainer_scenario(image = "vim-fixture")]
async fn vim_save_round_trip(at: AgentTui<'_>) -> Result<()> {
    let pane = at.spawn(["vim", "/fixtures/sample.txt"]).await?;
    at.wait(WaitCondition::Text { regex: r#""sample.txt""#.into() }).await?;
    at.press("i hello<esc>:w<cr>").await?;
    at.wait(WaitCondition::Text { regex: "written".into() }).await?;
    let snap = at.snapshot().await?;
    assert!(snap.outline.contains("hello"));
    at.die().await?;
    Ok(())
}
```

Pros: full Rust type checking, IDE completion, no DSL parser to maintain.
Cons: each new scenario is a chunk of Rust; less expressive at the "this
is a sequence of user actions" level.

### B. YAML/JSON script per scenario

```yaml
image: vim-fixture
steps:
  - spawn: [vim, /fixtures/sample.txt]
  - wait: { text: '"sample.txt"' }
  - press: "i hello<esc>:w<cr>"
  - wait: { text: "written" }
  - assert.snapshot.outline.contains: "hello"
  - die
```

Pros: scenarios are data; non-Rust contributors can add cases; nicely
self-documenting.
Cons: needs a runner (~200 LOC); type errors deferred to runtime; assert
DSL grows.

**Recommended:** **A first, B later** if scenario count outpaces what's
comfortable in Rust. The runner cost is real and we don't know how many
scenarios we'll end up with.

## Fixture images

One image per program family. Bake them in CI and publish to
`ghcr.io/conductorone/agent-tui-fixtures/<name>:<tag>` so test runs pull
deterministic versions. Initial set:

| Image | Contents | Scenarios |
|---|---|---|
| `shell-fixture` | bash + zsh + fish; OSC 133 shell-integration scripts pre-loaded | shell prompt detection, multi-shell switching |
| `vim-fixture` | vim + nvim + a sample repo for edits | edit + save, search, multi-buffer, alt-screen toggle |
| `lazygit-fixture` | lazygit + a seeded repo with branches | navigate commits, stage, commit |
| `k9s-fixture` | k9s + kind cluster seeded with CrashLoop fixture | list pods, filter, view logs |
| `claude-code-fixture` | a synthetic Ink banner emitter (mimics claude/codex) | claude-code adapter detection, banner-driven re-detect |
| `htop-fixture` | htop with deterministic process tree (`stress-ng` workers) | snapshot navigation, sort columns |

Dockerfiles are short (~30 lines each). Fixture seeds — config files,
seeded directories, the OSC 133 shell-integration script — live under
`tests/fixtures/<image>/` in this repo and get COPY-ed during the image
build.

## Debug affordances when a scenario fails

The instinct for these tests is they'll be flaky and frustrating to
debug. Build the debugging in from day one:

| When | What we capture | Where it lands |
|---|---|---|
| Test enters | container id, image tag, fixture commit | stdout via tracing |
| Each step | timestamp, command, response success/failure | per-scenario log file |
| Assertion fails | last 3 snapshots (outline + cells) | `target/integration-artifacts/<test>/snapshots.json` |
| Assertion fails | full cast file from `/var/agent-tui/<session>/<pane>.cast` | `target/integration-artifacts/<test>/p1.cast` |
| Assertion fails | container's stdout/stderr + `agent-tui daemon status` JSON | same dir |
| Assertion fails | `agent-tui doctor --diagnostic-bundle` tarball | same dir |
| Test panics | all of the above, captured by a `Drop` impl on the test handle | same dir |
| CI run | the entire `target/integration-artifacts/` directory uploaded as an action artifact | GitHub Action artifact "integration-debug" |

The `Drop` capture is the trick — assertion macros panic, the test
handle's `Drop` runs, and the artifact dir is written **before** the
container is torn down. This is the same pattern playwright uses
(`testInfo.attach`).

## Performance + flakiness budget

| Metric | Target | Rationale |
|---|--:|---|
| Cold image pull (first time) | < 60 s per image | Six images cached after the first run |
| Warm container start | < 2 s | testcontainers reuses layer cache |
| Per-scenario wall time (including container start) | < 10 s | fits inside a single CI job slot |
| Total integration suite | < 5 min | comparable to current `cargo test` |
| Flake budget | < 1% over 100 consecutive runs | enforce by retrying failed scenarios once in CI |

If a scenario flakes more than that, it's a defect — either in agent-tui
or in the fixture — and we fix the root cause rather than tune the
sleep.

## Container runtime: Docker OR Podman, transparently

testcontainers-rs talks to anything that implements the Docker HTTP API.
The runtime is selected by the `DOCKER_HOST` env var:

| Environment | `DOCKER_HOST` value |
|---|---|
| Local dev w/ Docker Desktop | unset (default `unix:///var/run/docker.sock`) |
| Local dev w/ Podman (rootless) | `unix:///run/user/$UID/podman/podman.sock` (after `podman system service --time=0`) |
| Linux CI on GitHub Actions | unset (Docker pre-installed) |
| Dev inside a sandboxed container (e.g. our Squire env) | Use Podman in rootless nested mode, or skip integration tests with `--features default` (i.e. don't enable the integration feature) |

No code change in our harness: `testcontainers::runners::AsyncRunner` reads
the env var. Document the Podman pattern in `CONTRIBUTING.md` so devs
without Docker can still run the suite.

This also de-risks the corporate-laptop / EKS-pod scenario — the moment
someone tries to develop where Docker can't run (locked-down macOS,
Linux sandbox, etc.) they can `apt install podman` and continue.

## Risks + mitigations

| Risk | Likelihood | Mitigation |
|---|---|---|
| Docker not available on dev boxes / corporate laptops | High | Mark integration tests as a separate target (`cargo test --features integration-docker` or a dedicated `tests/integration_docker/` directory). Default `cargo test` runs without them. CI runs the full set. |
| Image registry costs / rate limits | Low | Cache the built fixture images in CI; only rebuild when their Dockerfile changes (path-filtered workflow). |
| Slower CI feedback | Medium | Split into "fast" (unit + e2e Unix sockets) and "slow" (integration docker) jobs; PRs go green on fast; slow is required-for-merge but only blocks merge-queue. |
| ARM64 vs x86_64 image differences | Medium | Multi-arch images via `docker buildx`. Fixture work is mostly text; arch shouldn't matter. |
| Time-of-day flakiness from TUI repaint cadences | Medium | Every TUI scenario must use `wait --text` / `wait --hash` / `wait --idle` rather than `sleep`. Build a lint that bans `tokio::time::sleep` in `tests/integration_docker/`. |
| Container-internal clock vs host clock | Low | Don't depend on absolute timestamps in assertions; everything is sequence- or hash-based. |

## When in the roadmap?

I think this is the **highest-leverage next investment** — higher than P4
(distribution) and P5 (live preview / scroll history). Reasoning:

- Every subsequent phase (P3 auth vault, P4 distribution, P5 polish)
  ships under-tested without it. Right now, the only way to know that
  the agent-tui flow actually drives vim is to run it by hand.
- The fixture image lattice doubles as a **demo asset** for the README
  and skill-data templates — "here are the real flows agent-tui is
  designed for, here's how the agent calls map onto them."
- Distribution (P4) lands `npm install -g agent-tui` for end users. The
  fastest way to validate the binary on a fresh machine is to spawn the
  same fixture image and run a scenario. The infra carries over.
- Real-world tests are the only way to catch substrate-class regressions
  when we eventually swap `alacritty_terminal` → `wezterm-term` →
  `libghostty-vt`. Without them, those swaps are scary.

## Recommended cycle plan

| # | Scope | LOC | Tests added |
|---|---|--:|---|
| I1 | Scaffolding: `tests/integration_docker/` module gated behind `integration-docker` feature; `testcontainers` dep; an `AgentTui<'_>` handle with `spawn`/`press`/`wait`/`snapshot`/`die` async methods; `Drop`-time artifact capture | ~250 | 0 |
| I2 | First image: `shell-fixture`. One Dockerfile + 3 scenarios (bash prompt detect, zsh prompt detect, OSC 133 marker upgrade). Run on Linux CI only. | ~150 | 3 |
| I3 | `vim-fixture` + 3 vim scenarios. CI matrix gains `integration-docker` job (Linux only). | ~120 | 3 |
| I4 | `claude-code-fixture` (synthetic Ink banner) + 2 scenarios that exercise the first-bytes re-detect path. | ~100 | 2 |
| I5 | `lazygit-fixture` + 3 scenarios. macOS CI gains Docker; Windows still skips integration suite (Docker-on-Windows is a separate adventure). | ~150 | 3 |
| I6 | `k9s-fixture` + `claude-code` synthetic-banner image polish. Required-for-merge CI gate. | ~120 | 2 |

**Total: 6 cycles, ~890 LOC, ~13 scenarios.** Each cycle ships a working
slice. After I3 the suite catches real vim regressions; after I5 it's
the gating quality bar for distribution.

## Decisions for sign-off

1. **In-tree fixture Dockerfiles** vs pre-built `ghcr.io` images?
   *Recommend in-tree* — easier to evolve, builds are reproducible from
   a clean clone, CI builds-and-caches the images automatically.
2. **Plain Rust scenarios** vs YAML DSL?
   *Recommend plain Rust* — defer the DSL until scenario count justifies
   it.
3. **Required-for-merge** from cycle I3 onward, or only when the suite
   is "mature"?
   *Recommend required-for-merge from I3*, with the "retry-once on
   flake" CI affordance to absorb the inevitable transient Docker hiccup.
4. **Slot in before or after P4 distribution?**
   *Recommend before* — P4 wants high confidence in the binary, and
   this is how we get it.

## What this isn't

- It's not a substitute for unit tests. The agent-tui machinery (engine,
  wait, governance) still needs the existing crate-level tests.
- It's not a substitute for the benchmark suite the RFC §18 calls for.
  Benchmarks measure performance; integration tests measure correctness.
  They share fixtures but run separately.
- It's not Playwright-for-terminals. We don't auto-record interactions
  and there's no GUI runner — just `cargo test --features
  integration-docker` and read the artifacts on failure.

## Next steps if we say yes

1. Land the current PR (#1) cleanly first — integration tests on a
   moving substrate is a recipe for thrash.
2. Open a follow-on PR: cycle I1 (scaffolding + dummy scenario asserting
   `docker run hello-world` succeeds, just to prove the harness is
   wired correctly).
3. Move to I2 (`shell-fixture`) once the harness is proven.

This document moves to `docs/integration-test-ecosystem.md` (drop the
"research/" prefix) and gets promoted to a real RFC after I1 lands.
