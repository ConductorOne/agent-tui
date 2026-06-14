---
name: ai-cli
description: Drive AI CLIs (opencode, pi, codex, claude-code, …) — auth, streaming, tool use, sessions
allowed-tools: Bash(agent-tui:*)
---

# AI CLIs (opencode, pi, codex, claude-code, …)

agent-tui can drive other AI coding agents as the PTY app under it
— useful when you want a meta-agent orchestrating sub-agents, when
you want to record/replay an AI session for audit, or when you want
the snapshot+ref interaction model on top of another AI CLI's UI.

The basic loop is the same as [core](../core/SKILL.md): spawn the
CLI, wait for its prompt, snapshot, interact. What changes is what
you're waiting *for* — AI CLIs have characteristic UI shapes that
make detection robust.

## Outline shape

Claude Code, Codex, and Pi have provider-specific terminal manifests.
They share a prompt/response shape, but their refs are intentionally
not collapsed into one namespace:

```
@claude                  role=root
├── @claude.response     role=response
├── @claude.input        role=input      focused=true when the cursor is there
├── @claude.approval     role=approval-request
├── @claude.tool         role=tool-call
├── @claude.file-change  role=file-change
└── @claude.done         role=done

@codex                   role=root
└── same child refs under @codex

@pi                      role=root
└── same child refs under @pi
```

The signal refs are regex-derived from the visible terminal screen.
They are enough for supervising a live session, but they are not the
same thing as SDK/harness event streams with structured permission or
tool-result objects.

Batch modes are intentionally different. `pi --print`, `codex exec`,
and Claude print/help/version invocations are stdout-producing commands,
not long-lived live agent screens, so the bundled provider manifests do
not claim them. Capture those with the generic shell/stdout path.

Read `agent-tui skills get addressing` for selector syntax. Common
patterns:

| Goal | Selector |
|---|---|
| Wait for Codex to be ready for input | `@codex.input[focused]` |
| Snapshot just the Codex reply | `@codex.response` |
| Wait for approval | `@codex.approval` |
| Wait for a file edit | `@codex.file-change` |
| Wait for completion | `@codex.done` |
| Get the input prompt's current line | `@codex.input` (read `.name`) |

For Aider/opencode-style screens that do not yet have provider
manifests, the older shared fallback may expose `@ai-cli.response`
and `@ai-cli.input`. Prefer provider refs when they exist.

## Agents controlling agents
<!-- tested-by: drives_long_running_inner_agent_to_file_change_and_done -->

The useful case is a live session over time: prompt, wait for a plan,
approve, verify a file change, run checks, and wait for done.

```bash {test=ai-cli-codex-long-session}
agent-tui spawn -- codex
agent-tui wait --ref '@codex.input[focused]'
agent-tui type --to '@codex.input' 'plan the release note and wait for approval'
agent-tui press --to '@codex.input' '<cr>'

agent-tui wait --ref '@codex.approval'
agent-tui type --to '@codex.input' 'approve'
agent-tui press --to '@codex.input' '<cr>'

agent-tui wait --ref '@codex.file-change'
agent-tui type --to '@codex.input' 'run checks'
agent-tui press --to '@codex.input' '<cr>'

agent-tui wait --ref '@codex.done'
```

## Read [core](../core/SKILL.md) first
<!-- tested-by: navigation -->

This skill assumes the core loop. It covers AI-CLI-specific
patterns: auth, streaming detection, session DBs, tool use.

## Driving Pi (earendil-works/pi)
<!-- tested-by: pi_says_hello_via_fake_inference -->

Pi is a single-turn-friendly OpenAI-compatible CLI. Its `--print`
mode runs non-interactively, prints the reply to stdout, exits.

```bash {test=ai-cli-pi-print}
agent-tui spawn -- pi --provider openai --model gpt-4o-mini --print "say hello"
agent-tui wait --text "\$"                  # back to shell prompt after pi exits
agent-tui snapshot                          # captures pi's printed reply
```

Pi reads provider config from `$PI_CODING_AGENT_DIR/models.json`.
For multi-provider setups, configure providers there ahead of time.

## Driving OpenCode (sst/opencode)
<!-- tested-by: opencode_persists_streamed_response_to_session_db -->

OpenCode is interactive by default but has a `run` subcommand for
non-interactive use:

```bash {test=ai-cli-opencode-run}
agent-tui spawn -- bash -c "cd /work && exec opencode run -m openai/o3-mini 'refactor this'"
agent-tui wait --text "build · o3-mini"     # session header
# OpenCode's `run --format default` doesn't write the body to stdout
# — see "Reading the assistant's reply" below.
```

**Critical flags for non-interactive automation:**
- `--dangerously-skip-permissions` — auto-approve tool calls;
  without it, opencode prompts on the TTY and deadlocks under a
  programmatic PTY.
- `--title 'fixed string'` — opencode normally fires a hidden
  title-generation request to the model *before* the real prompt;
  pass a fixed title to skip it.
- `--pure` — skip plugins (deterministic).

Run `cd /work && exec opencode …` so the per-project `opencode.json`
gets picked up (opencode looks for it in the current directory).

## Reading the assistant's reply
<!-- tested-by: opencode_persists_streamed_response_to_session_db -->

Two strategies, depending on the CLI:

**1. CLI writes the reply to stdout (Pi, claude-code, `--format json`):**

```bash
agent-tui snapshot                          # body is on the screen
```

**2. CLI writes only a header, persists the body to a session DB
(OpenCode `--format default`):**

OpenCode persists each session at
`~/.local/share/opencode/opencode.db` (SQLite, WAL mode). After the
process exits, read the `part` table:

```bash {test=ai-cli-read-session-db}
sqlite3 ~/.local/share/opencode/opencode.db \
  "SELECT data FROM part WHERE message_id IN (
     SELECT id FROM message ORDER BY time_created DESC LIMIT 1)"
```

Or, in WAL mode immediately after a fresh run (data may not yet be
checkpointed), grep both files:

```bash
grep -a "MARKER_TEXT" ~/.local/share/opencode/opencode.db \
                     ~/.local/share/opencode/opencode.db-wal
```

## Streaming responses
<!-- tested-by: opencode_streams_chunks_incrementally -->

For visible-progress streaming, prefer a CLI mode that writes events
to stdout as they arrive:

```bash {test=ai-cli-stream-progress}
agent-tui spawn -- bash -c "cd /work && exec opencode run --format json 'long task'"
# OpenCode emits one JSON event per line as the model streams.
# agent-tui's wait --text matches against each line as it lands.
agent-tui wait --text "\"type\":\"text\""   # first text delta
```

This is much more reliable than mid-render screenshots — JSON
deltas are deterministic, screen contents during a stream are not.

## Tool use round-trips
<!-- tested-by: opencode_executes_bash_tool_use_and_persists_output -->

When the model calls a tool (bash, edit, glob, …), the CLI executes
it locally then sends the result back in a follow-up request. The
full loop:

1. Model returns `function_call(bash, {"command":"…","description":"…"})`.
2. CLI runs the command. With `--dangerously-skip-permissions` no
   prompt blocks; otherwise the CLI shows a confirm dialog.
3. CLI posts a 2nd request with `function_call_output` containing
   the stdout.
4. Model returns the final assistant text.
5. CLI persists tool call + output + assistant text to its session DB.

```bash {test=ai-cli-tool-use-from-outside}
agent-tui spawn -- bash -c "cd /work && exec opencode run \
  --dangerously-skip-permissions \
  -m openai/o3-mini 'find files matching pattern X'"
agent-tui wait --text "build · "
# The bash tool runs inside the same /work dir agent-tui spawned in.
# After exit, the DB has the full call → output → reply chain.
```

**OpenCode bash tool schema** requires BOTH `command` AND
`description` keys in the arguments JSON. The model usually emits
both correctly; if you're hand-crafting the args (test scenarios),
omitting either yields a SchemaError instead of running the command.

## Long-lived sessions
<!-- tested-by: pi_multi_request_advances_script -->

Most AI CLIs persist a session ID that lets you resume later:

```bash
# OpenCode
agent-tui spawn -- bash -c "cd /work && exec opencode run --continue 'follow-up question'"
agent-tui spawn -- bash -c "cd /work && exec opencode run --session <ID> 'specific session'"

# Pi
agent-tui spawn -- pi --resume <session-id> --print "follow-up"
```

The session ID surfaces in stdout on creation. For OpenCode it also
appears in the DB's `session` table.

## When the agent reports done
<!-- tested-by: opencode_handles_server_500_without_hanging -->

Detection options, in order of robustness:

1. **Process exit** — `agent-tui wait --exit` (P2) — best, no ambiguity.
2. **Return to the shell prompt** — `agent-tui wait --text "\$ "`
   — works when you spawned `bash -c "agent …"` rather than the
   agent directly.
3. **CLI's "done" marker** — opencode's session header redraw,
   pi's `\n` flush, claude-code's `>` prompt — fragile to version
   changes.

Don't use `--idle` as the primary "done" signal — long-running
generations have legitimate quiet periods.

## Authentication
<!-- tested-by: untested (auth-vault landing in P3; today you wire credentials through env vars or per-CLI config files like ~/.config/opencode/auth.json) -->

Today, set credentials before spawn:

```bash
agent-tui spawn --env OPENAI_API_KEY=sk-… --env ANTHROPIC_API_KEY=… -- opencode run …
```

Or write the CLI's own credentials file under `$HOME` before
spawning. Per-CLI:

| CLI | Credentials path |
|---|---|
| OpenCode | `~/.config/opencode/auth.json` |
| Pi | `$PI_CODING_AGENT_DIR/models.json` |
| Claude Code | `~/.claude.json` |
| Codex | `~/.config/codex/credentials.json` |

The P3 auth vault will provide a single `agent-tui auth save <name>`
API across CLIs.

## Testing AI-CLI integrations hermetically
<!-- tested-by: navigation -->

Driving an AI CLI in a TEST should not hit a real model. agent-tui
ships an in-process **fake-inference server** (under
`crates/agent-tui-integration/src/fake_inference.rs`) that speaks
the OpenAI Chat Completions API and the Responses API. Point the
CLI's `baseURL` at it and script canned replies:

```rust
// In an integration test:
let server = FakeServer::start(
    Script::new()
        .stream(["Hello ", "from ", "the fake"])
        .tool_call("bash", r#"{"command":"ls /","description":"list"}"#)
        .say("Done.")
).await?;
// CLI under test points at server.url(), runs, asserts against
// the session DB or stdout.
```

See `crates/agent-tui-integration/tests/opencode_fake_inference.rs`
and `pi_fake_inference.rs` for working scenarios. Lessons learned:

- **sha256-pin the CLI binary** in your Dockerfile. Don't use
  `latest`.
- **Pre-install ripgrep** for CLIs that bundle it (OpenCode
  symlinks at `~/.cache/opencode/bin/rg`).
- **Persist `$HOME`** when you need to inspect the session DB
  post-run.
- **Read `.db-wal` too** for content written but not yet
  checkpointed.

## When NOT to use this skill
<!-- tested-by: navigation -->

- **Shell sessions:** [shell](../shell/SKILL.md).
- **vim/nvim editing:** [vim](../vim/SKILL.md).
- **htop, less, fzf, lazygit, tig, nano:** [tui-apps](../tui-apps/SKILL.md).
