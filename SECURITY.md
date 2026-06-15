# Security Policy

## Reporting a vulnerability

Report security issues privately. Do not open a public issue for a suspected
vulnerability.

- Preferred: open a private advisory via GitHub Security Advisories
  ("Report a vulnerability" on the repository's Security tab).
- Alternatively, email `security@conductorone.com`.

Please include enough detail to reproduce — version (`agent-tui --version`),
platform, the command sequence, and the observed vs. expected behavior. We will
acknowledge receipt, work with you on a fix, and coordinate a disclosure
timeline.

## Supported versions

agent-tui is pre-1.0. Security fixes land on the latest release; there is no
backport guarantee for older versions. Pin a release tag and upgrade to pick up
fixes.

| Version | Supported |
|---|---|
| Latest release | Yes |
| Older releases | No |

## Security model

agent-tui spawns and drives terminal programs on a PTY: it executes child
processes and injects keystrokes into them. Treat it as a tool that runs code
on the host with the privileges of the user who invokes it. It is not a
sandbox, container runtime, or permission boundary.

The `--allowed-binaries` option can restrict which program names `spawn` will
launch. It does not constrain the behavior of a launched program, and it does
not make an untrusted program safe to run. If the target program needs
isolation, run agent-tui and the target inside a sandbox you already trust.

The daemon exposes a local control surface for a session. If another process
can access that session's socket and state as the same user, assume it can
observe or drive the session. Screen snapshots, casts, and logs can contain
anything the target program rendered; treat them as sensitive when the terminal
session handles sensitive data.

Valid security reports include:

- bypassing `--allowed-binaries` to launch a disallowed program;
- one session or user unexpectedly controlling another session;
- unsafe permissions or storage behavior that exposes snapshots, casts, logs,
  or socket access beyond the invoking user;
- input routed to the wrong pane or target after a command was accepted;
- crashes or unbounded resource use caused by terminal output that should be
  handled as data.

The following are normally not security vulnerabilities in agent-tui:

- a spawned program can read, write, execute, or access the network as the
  invoking user;
- agent-tui can be instructed by an authorized caller to type destructive input
  into a running program;
- a target program can print misleading or instruction-shaped text on its
  screen;
- a sandbox escape in a sandbox that agent-tui did not provide.
