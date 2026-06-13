# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.12](https://github.com/ConductorOne/agent-tui/compare/v0.1.11...v0.1.12) - 2026-06-13

### Added

- *(ux)* selector errors include caret + available refs
- *(adapter)* claude-code emits @ai-cli hierarchical durable refs ([#6](https://github.com/ConductorOne/agent-tui/pull/6))
- DOM-lite addressing model (selectors + refs + routed writes) ([#5](https://github.com/ConductorOne/agent-tui/pull/5))

### Other

- Merge pull request #98 from ConductorOne/release-plz-2026-06-13T00-26-48Z
- *(agent-tui)* release v0.1.11
- *(agent-tui)* release v0.1.10
- Merge pull request #94 from ConductorOne/release-plz-2026-06-10T14-30-15Z
- *(agent-tui)* release v0.1.9
- *(agent-tui)* release v0.1.8 ([#91](https://github.com/ConductorOne/agent-tui/pull/91))
- Add in-place daemon upgrade (Option-A re-exec) — U1 ([#86](https://github.com/ConductorOne/agent-tui/pull/86))
- *(agent-tui)* release v0.1.7 ([#84](https://github.com/ConductorOne/agent-tui/pull/84))
- Merge pull request #83 from ConductorOne/release-plz-2026-06-06T18-27-46Z
- *(agent-tui)* release v0.1.6
- *(agent-tui)* release v0.1.5
- *(agent-tui)* release v0.1.4
- Merge pull request #42 from ConductorOne/release-plz-2026-06-03T23-54-03Z
- *(agent-tui)* release v0.1.3
- *(agent-tui)* release v0.1.2
- *(agent-tui)* release v0.1.1
- subprocess-as-data model + adapter manifests + skills system
- layered child-process cleanup architecture (Layers 1/3/4)
- server mode wired — agent-tui drives panes from Claude Desktop
- swap tokio UnixListener/UnixStream for interprocess on all platforms
- P3 core: typed Action governance + nonced delimiters + audit firehose
- Pay down deferred work: focus, markers, checkpoints, first-bytes, OSC 133
- wait subsystem, cells/hybrid snapshots, state classifier, recorder
- P0 closure: wire doctor --quick to the daemon
- real engine, PTY, registry, spawn/die/list/snapshot
- initial scaffolding: workspace, RFC, daemon + CLI skeleton

## [0.1.11](https://github.com/ConductorOne/agent-tui/compare/v0.1.10...v0.1.11) - 2026-06-13

### Fixed

- *(wait)* exit 124 on timeout per the ratified contract ([#93](https://github.com/ConductorOne/agent-tui/pull/93))

## [0.1.10](https://github.com/ConductorOne/agent-tui/compare/v0.1.9...v0.1.10) - 2026-06-10

### Added

- *(ux)* selector errors include caret + available refs
- *(adapter)* claude-code emits @ai-cli hierarchical durable refs ([#6](https://github.com/ConductorOne/agent-tui/pull/6))
- DOM-lite addressing model (selectors + refs + routed writes) ([#5](https://github.com/ConductorOne/agent-tui/pull/5))

### Other

- Merge pull request #94 from ConductorOne/release-plz-2026-06-10T14-30-15Z
- *(agent-tui)* release v0.1.9
- *(agent-tui)* release v0.1.8 ([#91](https://github.com/ConductorOne/agent-tui/pull/91))
- Add in-place daemon upgrade (Option-A re-exec) — U1 ([#86](https://github.com/ConductorOne/agent-tui/pull/86))
- *(agent-tui)* release v0.1.7 ([#84](https://github.com/ConductorOne/agent-tui/pull/84))
- Merge pull request #83 from ConductorOne/release-plz-2026-06-06T18-27-46Z
- *(agent-tui)* release v0.1.6
- *(agent-tui)* release v0.1.5
- *(agent-tui)* release v0.1.4
- Merge pull request #42 from ConductorOne/release-plz-2026-06-03T23-54-03Z
- *(agent-tui)* release v0.1.3
- *(agent-tui)* release v0.1.2
- *(agent-tui)* release v0.1.1
- subprocess-as-data model + adapter manifests + skills system
- layered child-process cleanup architecture (Layers 1/3/4)
- server mode wired — agent-tui drives panes from Claude Desktop
- swap tokio UnixListener/UnixStream for interprocess on all platforms
- P3 core: typed Action governance + nonced delimiters + audit firehose
- Pay down deferred work: focus, markers, checkpoints, first-bytes, OSC 133
- wait subsystem, cells/hybrid snapshots, state classifier, recorder
- P0 closure: wire doctor --quick to the daemon
- real engine, PTY, registry, spawn/die/list/snapshot
- initial scaffolding: workspace, RFC, daemon + CLI skeleton

## [0.1.9](https://github.com/ConductorOne/agent-tui/compare/v0.1.8...v0.1.9) - 2026-06-10

### Added

- screen-model query verbs (snapshot fields, events, capabilities, wait exit code)

## [0.1.8](https://github.com/ConductorOne/agent-tui/compare/v0.1.7...v0.1.8) - 2026-06-08

### Other

- Add in-place daemon upgrade (Option-A re-exec) — U1 ([#86](https://github.com/ConductorOne/agent-tui/pull/86))

## [0.1.7](https://github.com/ConductorOne/agent-tui/compare/v0.1.6...v0.1.7) - 2026-06-06

### Added

- *(ux)* selector errors include caret + available refs
- *(adapter)* claude-code emits @ai-cli hierarchical durable refs ([#6](https://github.com/ConductorOne/agent-tui/pull/6))
- DOM-lite addressing model (selectors + refs + routed writes) ([#5](https://github.com/ConductorOne/agent-tui/pull/5))

### Other

- Merge pull request #83 from ConductorOne/release-plz-2026-06-06T18-27-46Z
- *(agent-tui)* release v0.1.6
- *(agent-tui)* release v0.1.5
- *(agent-tui)* release v0.1.4
- Merge pull request #42 from ConductorOne/release-plz-2026-06-03T23-54-03Z
- *(agent-tui)* release v0.1.3
- *(agent-tui)* release v0.1.2
- *(agent-tui)* release v0.1.1
- subprocess-as-data model + adapter manifests + skills system
- layered child-process cleanup architecture (Layers 1/3/4)
- server mode wired — agent-tui drives panes from Claude Desktop
- swap tokio UnixListener/UnixStream for interprocess on all platforms
- P3 core: typed Action governance + nonced delimiters + audit firehose
- Pay down deferred work: focus, markers, checkpoints, first-bytes, OSC 133
- wait subsystem, cells/hybrid snapshots, state classifier, recorder
- P0 closure: wire doctor --quick to the daemon
- real engine, PTY, registry, spawn/die/list/snapshot
- initial scaffolding: workspace, RFC, daemon + CLI skeleton

## [0.1.6](https://github.com/ConductorOne/agent-tui/compare/v0.1.5...v0.1.6) - 2026-06-06

### Added

- *(lifecycle)* holistic exit-code lifecycle — remembered code + fate-fidelity ([#60](https://github.com/ConductorOne/agent-tui/pull/60))
- *(attach)* atomic rendered-prelude + byte-follow + write-lease ([#58](https://github.com/ConductorOne/agent-tui/pull/58))
- *(snapshot)* real --png rasterization + --annotate ref-overlay ([#56](https://github.com/ConductorOne/agent-tui/pull/56))
- *(list)* report live pane size instead of stale spawn-time geometry ([#54](https://github.com/ConductorOne/agent-tui/pull/54))
- *(die)* group-aware graceful teardown (`die --grace`) — fixes orphan hazard ([#52](https://github.com/ConductorOne/agent-tui/pull/52))

### Fixed

- *(daemon)* reap PTY children on shutdown so owner death never leaks (cov-7) ([#74](https://github.com/ConductorOne/agent-tui/pull/74))

### Other

- *(cli)* streaming-verb exit-status mirroring matrix (cov-6) ([#72](https://github.com/ConductorOne/agent-tui/pull/72))

## [0.1.5](https://github.com/ConductorOne/agent-tui/compare/v0.1.4...v0.1.5) - 2026-06-04

### Other

- update Cargo.toml dependencies

## [0.1.4](https://github.com/ConductorOne/agent-tui/compare/v0.1.3...v0.1.4) - 2026-06-04

### Other

- Merge pull request #42 from ConductorOne/release-plz-2026-06-03T23-54-03Z

## [0.1.3](https://github.com/ConductorOne/agent-tui/compare/v0.1.2...v0.1.3) - 2026-06-03

### Added

- *(mcp)* expose snapshot `text` mode over MCP + truthful MCP docs (RFC P0-1) ([#35](https://github.com/ConductorOne/agent-tui/pull/35))
- *(cli)* per-command read timeout, snapshot --keep-color, session gc ([#21](https://github.com/ConductorOne/agent-tui/pull/21))

### Fixed

- *(cli,docs)* `--sequence` visible alias of `--since` + enforce commands.md↔--help conformance (RFC P1-1) ([#39](https://github.com/ConductorOne/agent-tui/pull/39))
- *(skill-data)* correct broken `less` wait recipe + audit spawn→wait ready-signals (RFC P0-2) ([#37](https://github.com/ConductorOne/agent-tui/pull/37))

### Other

- *(cli,skill)* truth-in-docs pass — timeout defaults, (unimplemented) tags, mcp help, decision table (RFC P1-2/P2/P3) ([#41](https://github.com/ConductorOne/agent-tui/pull/41))
- *(gc,ring)* real-system e2e for `session gc` CLI + OutputRing eviction ([#31](https://github.com/ConductorOne/agent-tui/pull/31))
- *(replay,edit)* real-system e2e for the `replay` and `edit` verbs ([#29](https://github.com/ConductorOne/agent-tui/pull/29))
- *(mcp)* real-system e2e for `mcp serve` JSON-RPC-over-stdio ([#27](https://github.com/ConductorOne/agent-tui/pull/27))
- *(streaming)* real-system e2e for `watch` / `tail --follow` ([#25](https://github.com/ConductorOne/agent-tui/pull/25))
- *(run)* real-system e2e for the `run` orchestration verb ([#23](https://github.com/ConductorOne/agent-tui/pull/23))

## [0.1.2](https://github.com/ConductorOne/agent-tui/compare/v0.1.1...v0.1.2) - 2026-05-29

### Added

- *(ux)* selector errors include caret + available refs
- *(adapter)* claude-code emits @ai-cli hierarchical durable refs ([#6](https://github.com/ConductorOne/agent-tui/pull/6))
- DOM-lite addressing model (selectors + refs + routed writes) ([#5](https://github.com/ConductorOne/agent-tui/pull/5))

### Other

- *(agent-tui)* release v0.1.1
- subprocess-as-data model + adapter manifests + skills system
- layered child-process cleanup architecture (Layers 1/3/4)
- server mode wired — agent-tui drives panes from Claude Desktop
- swap tokio UnixListener/UnixStream for interprocess on all platforms
- P3 core: typed Action governance + nonced delimiters + audit firehose
- Pay down deferred work: focus, markers, checkpoints, first-bytes, OSC 133
- wait subsystem, cells/hybrid snapshots, state classifier, recorder
- P0 closure: wire doctor --quick to the daemon
- real engine, PTY, registry, spawn/die/list/snapshot
- initial scaffolding: workspace, RFC, daemon + CLI skeleton

## [0.1.1](https://github.com/ConductorOne/agent-tui/compare/v0.1.0...v0.1.1) - 2026-05-28

### Other

- update Cargo.toml dependencies
