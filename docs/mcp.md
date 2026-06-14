# MCP server

`agent-tui mcp serve` exposes agent-tui's pane-driving surface as
[Model Context Protocol](https://modelcontextprotocol.io) tools over stdio, so
an MCP client — Claude Desktop, Claude Code, or any other — can spawn terminal
programs and drive them.

## Transport

The server speaks JSON-RPC 2.0 over stdio, one message per line (NDJSON; no
`Content-Length` headers). It advertises protocol version `2024-11-05` in the
`initialize` handshake. The first tool call that needs a daemon starts one
lazily, so there is nothing to start ahead of time.

## Tools

`tools/list` returns:

| Tool | What it does |
|---|---|
| `spawn` | Spawn a PTY-backed pane running an argv |
| `list` | List sessions and panes |
| `snapshot` | Read a pane (`outline` / `text` / `cells` / `adapter` / `hybrid`) |
| `press` | Press a key-token sequence (`"i hello<esc>:w<cr>"`) |
| `type` | Type literal text |
| `wait` | Block until a screen-state condition holds |
| `die` | Close a pane |
| `focus` | Set the focused pane |
| `daemon_status` | Report the live daemon's version and state |

Each tool is a thin wrapper over the corresponding CLI command. Streaming
progress, non-text content, resource listings, and prompt templates are not
yet implemented.

## Claude Desktop

Add the server to `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "agent-tui": {
      "command": "agent-tui",
      "args": ["mcp", "serve"]
    }
  }
}
```

The config file lives at:

- macOS — `~/Library/Application Support/Claude/claude_desktop_config.json`
- Windows — `%APPDATA%\Claude\claude_desktop_config.json`

Use an absolute path for `command` if `agent-tui` is not on the GUI app's
`PATH`. Restart Claude Desktop to pick up the change.

## Claude Code

Register the server with the CLI:

```bash
claude mcp add agent-tui -- agent-tui mcp serve
```

Or add it by hand to `.mcp.json` (project) or your user config:

```json
{
  "mcpServers": {
    "agent-tui": {
      "command": "agent-tui",
      "args": ["mcp", "serve"]
    }
  }
}
```

## Generic MCP client

Any client that launches a stdio MCP server works: run `agent-tui mcp serve` as
the server command with no arguments. The client performs the `initialize`
handshake, sends `notifications/initialized`, then calls `tools/list` and
`tools/call`. Pass agent-tui's global flags after `serve` if you need a
non-default session, e.g. `agent-tui --session work mcp serve`.

## Verify

Drive one tool by hand to confirm the wiring:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | agent-tui mcp serve
```

The second response lists the tools above.
