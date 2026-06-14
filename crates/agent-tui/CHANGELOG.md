# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

This changelog summarizes user-facing changes. Patch releases with no
user-facing notes are omitted.

## [Unreleased]

## [0.2.0](https://github.com/ConductorOne/agent-tui/compare/v0.1.12...v0.2.0) - 2026-06-14

### Added

- Manifest v2 adapter manifests with durable provider refs for live agent CLIs:
  `@claude`, `@codex`, `@pi`, `@aider`, and `@opencode`.
- Screen-derived agent-control refs for prompts, responses, approval requests,
  file changes, and done states.
- Dynamic row selectors and signal refs for adapters that need to address live,
  changing terminal regions.
- PNG snapshots with palette-aware rendering, optional chrome, styled text
  attributes, and clearer annotations.
- Launch-ready README, demo recordings, and operator-facing docs.

### Changed

- Live AI CLI support is provider-specific. There is no shared `@ai-cli`
  fallback; unknown agent CLIs use generic terminal refs until they have a v2
  manifest or richer provider adapter.
- Batch invocations such as `pi --print`, `codex exec`, and `opencode run`
  stay on stdout/generic handling instead of being misclassified as live agent
  sessions.
- The launch release is staged as v0.2.0 because manifest v2 intentionally
  replaces the earlier manifest shape.

### Fixed

- Prevent top-row dynamic manifest selectors from underflowing in debug and
  coverage builds.
- Clean up changelog generation so future release notes avoid duplicate
  `Unreleased` sections, off-by-one attribution, and merge/release noise.

### Testing

- Added daemon regressions for provider-specific fake agent sessions, including
  a long-running fake Codex flow through plan, approval, file-change, and done.
- Added detection guards so batch-mode agent commands do not claim live
  provider manifests.
- Added AI SDK harness CLI-name coverage for Claude Code, Codex, and Pi style
  invocations.

## [0.1.12](https://github.com/ConductorOne/agent-tui/compare/v0.1.11...v0.1.12) - 2026-06-13

### Fixed

- `wait` exits with status 124 on timeout.

## [0.1.10](https://github.com/ConductorOne/agent-tui/compare/v0.1.9...v0.1.10) - 2026-06-10

### Added

- Screen-model query verbs for snapshot fields, events, capabilities, and wait
  exit codes.

## [0.1.8](https://github.com/ConductorOne/agent-tui/compare/v0.1.7...v0.1.8) - 2026-06-08

### Added

- In-place daemon upgrade support.

### Fixed

- Event-driven low-latency follower streaming.
- Adopted panes now reconcile correctly with the daemon output watch channel.

## [0.1.7](https://github.com/ConductorOne/agent-tui/compare/v0.1.6...v0.1.7) - 2026-06-06

### Added

- Group-aware graceful teardown with `die --grace`.
- Live pane dimensions in `list`.
- PNG snapshot rendering and annotated ref overlays.
- Atomic attach prelude, byte-follow streaming, and write-lease arbitration.
- Remembered child exit status across pane lifecycle operations.
- Native multi-arch GHCR binary builds.

### Fixed

- PTY children are reaped on daemon shutdown so owner death does not leak
  child processes.

### Testing

- Expanded real-system coverage for ring eviction, write leases, focused-pane
  ambiguity, routed `press --to`, streaming exit status, attach fan-out, and
  resize races.

## [0.1.6](https://github.com/ConductorOne/agent-tui/compare/v0.1.5...v0.1.6) - 2026-06-06

### Changed

- cargo-dist owns GitHub Release creation while release-plz manages versioning.

## [0.1.5](https://github.com/ConductorOne/agent-tui/compare/v0.1.4...v0.1.5) - 2026-06-04

### Changed

- Release automation uses a GitHub App token so tag pushes trigger cargo-dist.
- README rewritten for the released product and stripped of internal launch
  phrasing.

## [0.1.4](https://github.com/ConductorOne/agent-tui/compare/v0.1.3...v0.1.4) - 2026-06-04

### Added

- Per-command read timeouts.
- `snapshot --keep-color`.
- Session garbage collection.
- Snapshot `text` mode over MCP.
- Non-blocking workspace coverage reporting.

### Documentation

- Documented timeout defaults, MCP help, and supported command behavior.

### Fixed

- Corrected the `less` wait recipe and audited spawn-to-ready signals.
- `--sequence` is a visible alias of `--since`.
- CLI help and command docs are checked for conformance.

### Testing

- Added real-system coverage for `run`, `watch`, `tail --follow`, `mcp serve`,
  `replay`, `edit`, session GC, and output-ring eviction.

## [0.1.2](https://github.com/ConductorOne/agent-tui/compare/v0.1.1...v0.1.2) - 2026-05-29

### Documentation

- README installation instructions for prebuilt release artifacts.

## [0.1.0] - 2026-05-28

### Added

- Initial CLI and daemon for spawning, observing, and controlling PTY-backed
  terminal sessions.
- `spawn`, `die`, `list`, `snapshot`, `press`, `type`, `send-ansi`, `resize`,
  `signal`, `wait`, `run`, `watch`, `tail`, `replay`, `edit`, and `session gc`.
- Text, cell, and hybrid snapshots, plus recording/replay support.
- Selector/ref addressing, routed writes, and stable generic refs.
- Adapter registry and built-in adapters for generic terminal panes, shells,
  Vim, and early AI CLI panes.
- MCP server mode for driving panes from external agents.
- Docker and bubblewrap integration fixtures for common terminal applications.
- Distribution and release automation.

### Fixed

- Cross-platform PTY setup for macOS and Windows.
- Child-process cleanup across daemon shutdown, owner death, and signal paths.
- CI, integration-fixture, and release-pipeline reliability issues.

### Testing

- End-to-end scenario proving MCP can drive Vim through bubblewrap.
