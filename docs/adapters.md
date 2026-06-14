# Writing a TOML adapter

An adapter maps a program's screen to a structured outline so agents address
named regions (`@status`, `@list`) instead of raw row/column positions. For
the long tail of TUI apps — helix, k9s, btop, ranger, gitui — you can add
coverage with a TOML manifest, no Rust and no rebuild.

This takes about twenty minutes: drop a file, respawn the daemon, snapshot.

## How detection works

When a pane is spawned, every adapter scores its confidence against the pane.
The highest score wins; the built-in `generic` adapter scores `0.1`, so any
manifest that matches at all takes over. A manifest scores by, in order:

- `0.9` — `detect.argv0` exact-matches the process basename.
- `0.7` — `detect.argv_contains` substring-matches any argv element (handles
  wrapper scripts like `bash -c "lazygit ..."`).
- `0.85` — `detect.banner_regex` matches the first ~512 bytes of PTY output.

An empty `detect` section means the adapter never auto-matches.

## Where manifests live

At daemon startup, manifests are loaded from the first directory that exists:

1. `$AGENT_TUI_ADAPTERS_DIR` (explicit override)
2. `$XDG_CONFIG_HOME/agent-tui/adapters/`
3. `$HOME/.config/agent-tui/adapters/`

A user manifest overrides a bundled one with the same `name`. Non-`.toml`
files are skipped; a malformed manifest logs a warning and is ignored — it
never crashes the daemon. After editing, respawn the daemon to reload.

## The schema

```toml
name = "lazygit"                # adapter id; conventionally matches the filename stem

[detect]
argv0 = ["lazygit"]             # exact process-basename matches
argv_contains = ["lazygit"]     # substring matches against any argv element
banner_regex = '^lazygit '      # optional; matched against the first ~512 bytes

[[regions]]
name = "status"                 # informational display name
role = "status-bar"             # outline-node role
rows = [0, 2]                   # inclusive row range
cols = [0, -1]                  # optional; inclusive col range, defaults to full width

[[regions]]
name = "files"
role = "list"
rows = [3, -2]                  # negative indices count from the end (-1 = last row)

[[regions]]
name = "footer"
role = "footer"
rows = [-1, -1]
```

Row and column ranges are inclusive. Negative indices count from the end, so
`-1` is the last row and `[-1, -1]` is the bottom line. Each region becomes one
outline node whose text is the rendered cells inside it; an empty region is
dropped, so a screen with only some regions filled emits no blank nodes.

## Write one

1. Spawn the app and read its raw layout to find the row bands:

   ```bash
   agent-tui spawn -- lazygit
   agent-tui wait --idle 300
   agent-tui snapshot --mode text
   ```

2. Map each visual band — header, body list, footer — to a `[[regions]]`
   entry with its row range and a sensible `role`.

3. Save as `~/.config/agent-tui/adapters/lazygit.toml`, then respawn:

   ```bash
   agent-tui die
   agent-tui spawn -- lazygit
   agent-tui snapshot --mode outline
   ```

   The outline's `adapter` field should now read `lazygit`, with one node per
   region.

## When a manifest is not enough

Manifests describe a static layout — fixed row bands mapped to nodes. For
behavior a TOML schema cannot express (live RPC, per-element refs, mode
detection, `eval`), implement the `Adapter` trait in Rust. See the built-in
adapters in `crates/agent-tui-adapter/src/builtin.rs` and the trait in
`crates/agent-tui-adapter/src/lib.rs`. For the selector grammar used to query
the resulting outline, run `agent-tui skills get addressing`.
