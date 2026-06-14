# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.13](https://github.com/ConductorOne/agent-tui/compare/0.1.12...0.1.13) - 2026-06-14

### Added

- *(snapshot)* Ship-nice PNG renders — palette, chrome frame, attrs, restyled annotate

### Fixed

- *(release)* Configure git-cliff changelog generation (dedup + skip merge/release noise)
- *(release)* Correct off-by-one version attribution + doubled Unreleased in CHANGELOG

### Other

- *(launch)* Prepare public release readiness ([#105](https://github.com/ConductorOne/agent-tui/pull/105))


## [0.1.12](https://github.com/ConductorOne/agent-tui/compare/v0.1.11...v0.1.12) - 2026-06-13

### Fixed

- *(wait)* Exit 124 on timeout per the ratified contract ([#93](https://github.com/ConductorOne/agent-tui/pull/93))

## [0.1.11](https://github.com/ConductorOne/agent-tui/compare/v0.1.10...v0.1.11) - 2026-06-13

## [0.1.10](https://github.com/ConductorOne/agent-tui/compare/v0.1.9...v0.1.10) - 2026-06-10

### Added

- Screen-model query verbs (snapshot fields, events, capabilities, wait exit code)

## [0.1.9](https://github.com/ConductorOne/agent-tui/compare/v0.1.8...v0.1.9) - 2026-06-10

## [0.1.8](https://github.com/ConductorOne/agent-tui/compare/v0.1.7...v0.1.8) - 2026-06-08

### Fixed

- *(daemon)* Event-driven low-latency follower streaming ([#87](https://github.com/ConductorOne/agent-tui/pull/87))
- *(daemon)* Reconcile adopt path with output_tx watch channel (broken main) ([#90](https://github.com/ConductorOne/agent-tui/pull/90))

### Other

- Add in-place daemon upgrade (Option-A re-exec) — U1 ([#86](https://github.com/ConductorOne/agent-tui/pull/86))

## [0.1.7](https://github.com/ConductorOne/agent-tui/compare/v0.1.6...v0.1.7) - 2026-06-06

### Added

- *(die)* Group-aware graceful teardown (`die --grace`) — fixes orphan hazard ([#52](https://github.com/ConductorOne/agent-tui/pull/52))
- *(list)* Report live pane size instead of stale spawn-time geometry ([#54](https://github.com/ConductorOne/agent-tui/pull/54))
- *(snapshot)* Real --png rasterization + --annotate ref-overlay ([#56](https://github.com/ConductorOne/agent-tui/pull/56))
- *(attach)* Atomic rendered-prelude + byte-follow + write-lease ([#58](https://github.com/ConductorOne/agent-tui/pull/58))
- *(lifecycle)* Holistic exit-code lifecycle — remembered code + fate-fidelity ([#60](https://github.com/ConductorOne/agent-tui/pull/60))

### CI/CD

- *(ghcr)* Multi-arch binary-only GHCR publish workflow (GHCR-A) ([#80](https://github.com/ConductorOne/agent-tui/pull/80))
- *(ghcr)* Native per-arch runners + digest-merge, drop QEMU (GHCR-A2) ([#82](https://github.com/ConductorOne/agent-tui/pull/82))

### Fixed

- *(daemon)* Reap PTY children on shutdown so owner death never leaks (cov-7) ([#74](https://github.com/ConductorOne/agent-tui/pull/74))

### Testing

- *(pty)* Real-system ring-eviction lost_bytes coverage (cov-1) ([#62](https://github.com/ConductorOne/agent-tui/pull/62))
- *(server)* Idle-pane disconnect releases write-lease (cov-2) ([#64](https://github.com/ConductorOne/agent-tui/pull/64))
- *(pane)* Real-system resolve_focused >1-live ambiguity (cov-3) ([#66](https://github.com/ConductorOne/agent-tui/pull/66))
- *(lease)* Write-lease gate uniform across all 4 write verbs (cov-4) ([#68](https://github.com/ConductorOne/agent-tui/pull/68))
- *(routing)* Real-adapter `press --to` routed-delivery e2e (cov-5) ([#70](https://github.com/ConductorOne/agent-tui/pull/70))
- *(cli)* Streaming-verb exit-status mirroring matrix (cov-6) ([#72](https://github.com/ConductorOne/agent-tui/pull/72))
- *(attach)* Many-viewer fan-out under concurrent load (cov-8) ([#76](https://github.com/ConductorOne/agent-tui/pull/76))
- *(resize)* Resize-vs-live-stream race is safe (cov-9) ([#78](https://github.com/ConductorOne/agent-tui/pull/78))

## [0.1.6](https://github.com/ConductorOne/agent-tui/compare/v0.1.5...v0.1.6) - 2026-06-06

### CI/CD

- *(release-plz)* Let cargo-dist own the GitHub Release (git_release_enable=false)

## [0.1.5](https://github.com/ConductorOne/agent-tui/compare/v0.1.4...v0.1.5) - 2026-06-04

### CI/CD

- *(release-plz)* Use GitHub App token so tag push triggers cargo-dist

### Documentation

- *(readme)* Full revamp for the released product
- *(readme)* Scrub self-congratulatory / AI-slop phrasing

## [0.1.4](https://github.com/ConductorOne/agent-tui/compare/v0.1.3...v0.1.4) - 2026-06-04

### Added

- *(cli)* Per-command read timeout, snapshot --keep-color, session gc ([#21](https://github.com/ConductorOne/agent-tui/pull/21))
- *(mcp)* Expose snapshot `text` mode over MCP + truthful MCP docs (RFC P0-1) ([#35](https://github.com/ConductorOne/agent-tui/pull/35))

### CI/CD

- Add non-blocking workspace coverage job (cargo-llvm-cov) ([#33](https://github.com/ConductorOne/agent-tui/pull/33))

### Documentation

- *(cli,skill)* Truth-in-docs pass — timeout defaults, (unimplemented) tags, mcp help, decision table (RFC P1-2/P2/P3) ([#41](https://github.com/ConductorOne/agent-tui/pull/41))

### Fixed

- *(skill-data)* Correct broken `less` wait recipe + audit spawn→wait ready-signals (RFC P0-2) ([#37](https://github.com/ConductorOne/agent-tui/pull/37))
- *(cli,docs)* `--sequence` visible alias of `--since` + enforce commands.md↔--help conformance (RFC P1-1) ([#39](https://github.com/ConductorOne/agent-tui/pull/39))

### Testing

- *(run)* Real-system e2e for the `run` orchestration verb ([#23](https://github.com/ConductorOne/agent-tui/pull/23))
- *(streaming)* Real-system e2e for `watch` / `tail --follow` ([#25](https://github.com/ConductorOne/agent-tui/pull/25))
- *(mcp)* Real-system e2e for `mcp serve` JSON-RPC-over-stdio ([#27](https://github.com/ConductorOne/agent-tui/pull/27))
- *(replay,edit)* Real-system e2e for the `replay` and `edit` verbs ([#29](https://github.com/ConductorOne/agent-tui/pull/29))
- *(gc,ring)* Real-system e2e for `session gc` CLI + OutputRing eviction ([#31](https://github.com/ConductorOne/agent-tui/pull/31))

## [0.1.3](https://github.com/ConductorOne/agent-tui/compare/v0.1.2...v0.1.3) - 2026-06-03

## [0.1.2](https://github.com/ConductorOne/agent-tui/compare/v0.1.1...v0.1.2) - 2026-05-29

### Documentation

- *(readme)* Install via prebuilt release artifacts ([#16](https://github.com/ConductorOne/agent-tui/pull/16))

## [0.1.1](https://github.com/ConductorOne/agent-tui/compare/v0.1.0...v0.1.1) - 2026-05-28

## [0.1.0] - 2026-05-28

### Added

- DOM-lite addressing model (selectors + refs + routed writes) ([#5](https://github.com/ConductorOne/agent-tui/pull/5))
- *(adapter)* Claude-code emits @ai-cli hierarchical durable refs ([#6](https://github.com/ConductorOne/agent-tui/pull/6))
- *(ux)* Selector errors include caret + available refs
- *(generic-adapter)* Emit @generic.* durable refs
- *(release)* Dist + release-plz minimum viable pipeline

### CI/CD

- Fix macOS sun_path overflow + drop Windows from matrix
- Fix Windows unsafe + fmt nit; add testcontainers research spike
- Unblock integration jobs (bwrap share-net hatch + docker stderr capture) ([#4](https://github.com/ConductorOne/agent-tui/pull/4))

### Documentation

- Mini-RFC for Windows support strategy

### Fixed

- *(daemon)* Cross-platform pipe + ptsname for macOS, gate custom-stdin for Windows ([#3](https://github.com/ConductorOne/agent-tui/pull/3))
- *(release-plz)* Grant pull-requests: read to the release job ([#13](https://github.com/ConductorOne/agent-tui/pull/13))
- *(release)* Override dist's deprecated default runner labels ([#15](https://github.com/ConductorOne/agent-tui/pull/15))

### Other

- Initial scaffolding: workspace, RFC, daemon + CLI skeleton
- Real engine, PTY, registry, spawn/die/list/snapshot
- Keymap parser, press/type barrier, send_ansi, resize, signal
- P0 closure: wire doctor --quick to the daemon
- Wait subsystem, cells/hybrid snapshots, state classifier, recorder
- P2 partial: adapter registry + built-in adapters
- Pay down deferred work: focus, markers, checkpoints, first-bytes, OSC 133
- P3 core: typed Action governance + nonced delimiters + audit firehose
- Swap tokio UnixListener/UnixStream for interprocess on all platforms
- Signal mapping + .exe-aware basename + re-enable CI
- Cycle I1 scaffolding for the testcontainers ecosystem
- Rootful podman socket bootstrap for sandboxed dev envs
- Integration I2: shell-fixture Dockerfile + OSC 133 e2e scenarios
- Debug story + vim fixture + vimtutor walkthrough
- Hermetic NetworkMode=none via host_config_modifier
- Bwrap backend — OCI rootfs + bubblewrap, ~600x faster than docker
- Property tests + UTF-8 chunk-boundary buffer
- Lazygit fixture + 3 end-to-end scenarios
- VimAdapter — mode-aware structured outline for vim panes
- Server mode wired — agent-tui drives panes from Claude Desktop
- Surface MCP server mode for Claude Desktop / Claude Code users
- 5 new fixtures (less, htop, tig*, fzf, nano) + engine regression
- Tig scenarios now pass — root-caused two distinct issues
- Normalize LINES + COLUMNS env to match the PTY size
- Layered child-process cleanup architecture (Layers 1/3/4)
- Pi v0.75.5 end-to-end against fake-inference server (3/3 scenarios)
- Subprocess-as-data model + adapter manifests + skills system
- Repoint repository URLs to ConductorOne org

### Testing

- Headline scenario — MCP drives vim through bwrap end-to-end

