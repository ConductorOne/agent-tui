# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.4](https://github.com/ConductorOne/agent-tui/compare/v0.1.3...v0.1.4) - 2026-06-04

### Added

- *(ux)* selector errors include caret + available refs
- *(adapter)* claude-code emits @ai-cli hierarchical durable refs ([#6](https://github.com/ConductorOne/agent-tui/pull/6))
- DOM-lite addressing model (selectors + refs + routed writes) ([#5](https://github.com/ConductorOne/agent-tui/pull/5))

### Other

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
