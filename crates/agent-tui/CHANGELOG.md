# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.3](https://github.com/ConductorOne/agent-tui/compare/v0.1.2...v0.1.3) - 2026-06-03

### Added

- *(cli)* per-command read timeout, snapshot --keep-color, session gc ([#21](https://github.com/ConductorOne/agent-tui/pull/21))

### Other

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
