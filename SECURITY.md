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
on the host with the privileges of the user who invokes it. Two mechanisms
constrain what a caller can do; both are real today, with the caveats noted.

### Typed-action governance + binary allowlist

Every mutating request (spawn, input, eval) funnels through a single governance
choke point before it reaches the PTY. The daemon builds a typed action and
runs it through a configured evaluator, which returns Allow or Deny; a Deny
returns a `POLICY_DENIED` response without touching the PTY. Every decision is
emitted on an audit event channel.

Two evaluators ship today:

- A permissive evaluator that allows everything (the default when no allowlist
  is configured). This is the development baseline — it imposes no restriction.
- A binary allowlist evaluator (`--allowed-binaries`) that checks `spawn`
  actions against a set of permitted binaries. An empty allowlist allows
  everything; the explicit `*` wildcard allows any spawn but records it in the
  audit log. Input and eval actions pass through.

A policy-language evaluator (OPA/Rego) is planned but not yet shipped. The
allowlist is therefore the only enforcing evaluator available today, and it
gates spawns only — not the keystrokes sent to an already-running child. Run
agent-tui against untrusted targets only inside a sandbox you already trust.

### Nonced content-boundary delimiters

Snapshot output can be wrapped in per-snapshot content-boundary delimiters
whose nonce is generated fresh for each snapshot (e.g.
`<<<AGENT_TUI_OUTPUT_a7b3c91d>>>` … `<<<END_a7b3c91d>>>`). These let a calling
agent distinguish tool output from instructions in its own context window — a
prompt-injection mitigation. The nonce is unpredictable to the observed
program, so on-screen text cannot forge a closing delimiter. This reduces, but
does not eliminate, prompt-injection risk: the surrounding agent must actually
honor the boundaries.
