# Mini-RFC: Windows support strategy

> **Historical design note.** This document predates the public release of
> agent-tui and may not match current behavior. It is kept for design context.

**Status:** Draft. Targets v1 (P4 distribution).
**Author:** generated from PR #1 conversation.

## TL;DR

Windows works once we swap three cfg-gated pieces: **IPC** (Unix socket → named
pipe), **signal delivery** (`killpg` → `GenerateConsoleCtrlEvent`/Job Object
termination), and a few **path/comm assumptions**. None require an architecture
change. Estimate: ~2 cycles.

PTY itself isn't a blocker — `portable-pty` already uses ConPTY on Windows.

---

## What blocks Windows today

| Piece | Today | Windows reality |
|---|---|---|
| Daemon IPC | `tokio::net::UnixListener` + `UnixStream` | Not available |
| Signal handler | `nix::sys::signal::killpg(pgid, sig)` | No POSIX process groups; no `killpg` |
| `MasterPty::process_group_leader()` | Returns `pid_t` | Returns `None` on Windows |
| `XDG_RUNTIME_DIR` / `XDG_STATE_HOME` paths | Read directly | Don't exist; we already fall back via `dirs` crate |
| Auth vault (P3 follow-on, not in scope yet) | `mlock` + Linux keyring / macOS Keychain | Needs Windows Credential Manager |

Two things that **already work**:
- `portable-pty` on Windows uses ConPTY (Windows ≥ 10 1809). Spawn / read / write / resize all functional.
- `alacritty_terminal` is platform-agnostic (no fd/socket assumptions).

---

## Recommendation: `interprocess` for IPC + cfg-gated signals

### 1. IPC — `interprocess::local_socket`

Swap `tokio::net::Unix{Listener,Stream}` for
[`interprocess::local_socket::tokio`](https://docs.rs/interprocess). One trait
surface, three transports underneath:

| Platform | Transport |
|---|---|
| Linux / macOS / BSD | Unix domain socket |
| Windows | Named pipe (`\\.\pipe\agent-tui-<session>`) |
| (fallback) | Abstract Unix socket on Linux |

Why `interprocess` over hand-rolled `cfg`-modules:
- It's actively maintained, tokio integration is first-class, and the API is the same shape on both sides (listener+stream pair, AsyncRead/AsyncWrite). The alternative — a homegrown `mod platform { #[cfg(unix)] use ... #[cfg(windows)] use ... }` — saves a dep but doubles the surface area of socket discovery, permissioning, and tests.
- The named-pipe address is a string we already shape correctly (`<session>` is the basename today; the prefix changes from `<dir>/<session>.sock` to `\\.\pipe\agent-tui-<session>`).

What this changes in our code:
- `SocketLayout::socket` becomes a `LocalSocketName` (or stays a `PathBuf` and we adapt at bind time — the latter is less invasive).
- `client::one_shot` connects via the new API.
- `server::run_daemon` binds via the new API.
- Tests stop using `/tmp/...sock` paths and switch to `LocalSocketName::from_str(...)`.

Estimated change: ~150 LOC across `client.rs`, `server.rs`, `paths.rs`, and the two test helpers. No protocol or handler changes.

### 2. Signal delivery — cfg-gated module

Today's `handlers::signal::deliver` is already `#[cfg(unix)]` / `#[cfg(not(unix))]` split (the not-unix arm currently errors). The Windows arm becomes:

| Signal name | Windows mapping (**as implemented** — supersedes the original `GenerateConsoleCtrlEvent` plan) |
|---|---|
| `SIGINT` | Write ETX `0x03` to the ConPTY **input** via the master writer; conhost synthesizes a real `CTRL_C_EVENT` for the pane's console clients (the idiomatic ConPTY path used by Windows Terminal/wezterm). `GenerateConsoleCtrlEvent` does **not** work here — the child is on its own pseudoconsole and is not a process-group root, so the call reaches nothing. |
| `SIGBREAK` / `SIGQUIT` | **Rejected** with `INVALID_ARGS`: no pseudoconsole-input byte is translated into a `CTRL_BREAK_EVENT`, and `GenerateConsoleCtrlEvent` can't reach a child on its own pseudoconsole. Writing FS `0x1c` would be a silent no-op, so we don't claim success. Callers use `SIGINT` (interrupt) or `SIGTERM`/`die` (stop). |
| `SIGTERM` / `SIGKILL` | Descendant-tree kill via `taskkill /F /T /PID` (`PtyChild::kill` → `kill_tree_windows`). |
| `SIGHUP` / `SIGUSR*` / etc. | Reject with `INVALID_ARGS` — no analog |

ConPTY doesn't expose process groups: `SIGINT` targets the child's console via the ConPTY input, and terminate uses the `taskkill /F /T` descendant-tree kill (portable-pty's own kill is child-PID-only, so we reap the tree explicitly rather than rely on a Job Object).

Implemented in `handlers/signal.rs` (+ `pty.rs` for the tree kill), with a `windows-sys` workspace dep.

### 3. Comm normalization in adapter detect

`PaneInfo.comm` is currently `basename(argv[0])`. On Windows the basename is typically `bash.exe`, `claude.exe`, etc. The built-in adapters' lookup tables would miss those.

Fix: strip a trailing `.exe` (case-insensitively) before lookup, both in `basename()` (spawn handler) and in the lookup constants (or normalize at compare time).

Estimated change: ~20 LOC, no new dep.

### 4. State / runtime dir paths

Already using the `dirs` crate, which returns sensible Windows paths:
- Recorder: `%LOCALAPPDATA%\agent-tui\<session>\<pane>.cast`
- Sockets: `%LOCALAPPDATA%\agent-tui\sockets\` (only relevant if we ever fall back from named pipes; not by default)

No code change needed — the existing `state_dir()` already covers Windows via `dirs::state_dir()`.

---

## What we explicitly defer

- **Auth vault on Windows** — Windows Credential Manager via `keyring` crate or `windows-sys`. Lands when the auth vault itself lands; both are P3 follow-on, not blocking Windows runtime.
- **TCP fallback** — Originally floated as the simple option. Skipping it: named pipes are more secure (DACL-controlled), faster, and `interprocess` makes them as easy as Unix sockets. No reason to add a third transport.
- **Windows-specific telemetry / crash dumps** — P5 polish concern.

---

## Cycle plan

| # | Scope | Tests added | Est. LOC |
|---|---|---|--:|
| W1 | `interprocess` swap; verify Linux + macOS still green; re-enable `windows-latest` in CI but allow it to fail | reuse existing | ~150 |
| W2 | Cfg-gated Windows signal handler; `.exe` strip in adapter detect; promote `windows-latest` to required | `signal_term_on_windows_calls_terminate_process` (cfg-gated) | ~120 |

Total: 2 OODA cycles, ~270 LOC. Test count grows by ~3.

---

## Open questions

1. **Bind permissions on named pipes.** Default DACL is broad. Should the daemon explicitly restrict to the current user's SID? Probably yes; default safety. ~10 lines of `windows-sys` SecurityAttributes work.
2. **`interprocess` maturity.** Crate is fine but worth a security review before depending on it for the connection-acceptance loop. (Quick check: it's 1.6 MB total source, well-tested, widely used.)
3. **`cargo-dist` cross-compile** for `x86_64-pc-windows-msvc` — P4 work already in the roadmap. Adding the windows runtime support shouldn't be blocked on cargo-dist setup; the binary already cross-compiles, we just don't run integration tests there yet.
4. **Should ConPTY width changes propagate the same WINSIZE events** to the engine? On Unix the kernel sends SIGWINCH; on Windows ConPTY surfaces resizes via its own event. `portable-pty`'s `master.resize()` already handles both consistently, but the test for "child sees the new size" is unix-only today. Worth a `cfg`-gated test pair.

---

## Decision needed

Two yes/no choices:
- **A**: adopt `interprocess` (recommended), or roll our own cfg-gated IPC?
- **B**: do W1+W2 in PR #2 (alongside MCP/distribution P4 work), or as a focused Windows PR before P4?

My take: A=`interprocess`, B=focused Windows PR before P4. P4's npm/brew/cargo-install distribution depends on the binary working on Windows; landing IPC + signals first removes a class of "downloaded binary doesn't run" surprises during P4 distribution rollout.
