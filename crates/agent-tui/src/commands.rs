//! Subcommand dispatch — translates the clap-derived [`Cli`] into either a
//! daemon-side action (`daemon run`) or a one-shot client RPC against the
//! local daemon.

use agent_tui_daemon::{DaemonConfig, run_daemon};
use agent_tui_protocol::{Command, SessionId};
use anyhow::{Result, anyhow};

use crate::cli::{Cli, Command as CliCmd, DaemonAction, EngineKind, PaneAction, SessionAction};
use crate::client;
use crate::gc;

/// Top-level dispatch entry point.
pub async fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        CliCmd::Daemon(args) => match args.action {
            DaemonAction::Run {
                monitor_parent,
                idle_timeout_secs,
            } => run_foreground_daemon(&cli.globals, monitor_parent, idle_timeout_secs).await,
            DaemonAction::Status => one_shot_print(&cli.globals, Command::DaemonStatus).await,
            DaemonAction::Shutdown { force } => {
                one_shot_print(&cli.globals, Command::DaemonShutdown { force }).await
            }
        },
        CliCmd::Session(args) => match args.action {
            SessionAction::Gc {
                older_than_days,
                all,
                dry_run,
            } => session_gc(&cli.globals, older_than_days, all, dry_run).await,
        },
        CliCmd::Doctor(args) => doctor(&cli.globals, &args).await,
        CliCmd::Skills(args) => skills(&args),
        CliCmd::Mcp(_) => crate::mcp::serve(cli.globals).await,
        CliCmd::DumpSurface => dump_surface(),
        CliCmd::Ask {
            provider,
            prompt,
            max,
        } => ask_sugar(&cli.globals, &provider, prompt, max).await,
        CliCmd::Edit { path, editor } => edit_sugar(&cli.globals, path, editor).await,
        CliCmd::Watch { argv } => watch_sugar(&cli.globals, argv).await,
        CliCmd::Replay {
            cast,
            expect_snapshot,
            mode,
            cols,
            rows,
        } => replay_cast(&cli.globals, cast, expect_snapshot, mode, cols, rows).await,
        CliCmd::Run {
            stdin,
            stdin_file,
            max,
            raw,
            keep_daemon,
            cwd,
            env,
            argv,
        } => {
            run_orchestrate(
                &cli.globals,
                stdin,
                stdin_file,
                max,
                raw,
                keep_daemon,
                cwd,
                env,
                argv,
            )
            .await
        }
        CliCmd::Tail {
            pane,
            since,
            strip_ansi,
            follow: true,
        } => tail_follow(&cli.globals, pane, since, strip_ansi).await,
        // Everything else is a one-shot client RPC. The daemon currently
        // returns a friendly INTERNAL error for unwired ops; the CLI surfaces
        // that as a non-zero exit so callers can branch.
        other => {
            let cmd = cli_command_to_protocol(other)?;
            one_shot_print(&cli.globals, cmd).await
        }
    }
}

async fn run_foreground_daemon(
    g: &crate::cli::GlobalArgs,
    monitor_parent: Option<u32>,
    idle_timeout_secs: Option<u64>,
) -> Result<()> {
    let layout = client::layout_for(&g.session, g.socket_dir.as_deref());
    // `--allowed-binaries` is also wired to AGENT_TUI_ALLOWED_BINARIES via
    // clap's `env` attr; the lazy-spawn path in client.rs forwards that env
    // var to the daemon child so foreground and lazy invocations agree.
    let cfg = DaemonConfig {
        session: SessionId(g.session.clone()),
        layout,
        engine: g.engine.as_str().to_string(),
        binary_version: env!("CARGO_PKG_VERSION").to_string(),
        allowed_binaries: g.allowed_binaries.clone(),
        monitor_parent,
        idle_timeout_secs,
    };
    let handle = run_daemon(cfg).await?;
    handle.shutdown.notified().await;
    Ok(())
}

#[allow(clippy::too_many_lines)]
/// Orchestrate the `run` sugar verb. Bundles:
///   spawn --stdin pipe → optionally stdin --text + close-stdin →
///   wait --exit → tail --strip-ansi → die
///
/// Returns a single JSON envelope:
///
/// ```json
/// {
///   "exit_code": 0,
///   "stdout": "answer text",
///   "elapsed_ms": 2867,
///   "argv": [...]
/// }
/// ```
///
/// The daemon-side primitives stay one-call-per-command for power
/// users; `run` is purely client-side stitching.
#[allow(clippy::too_many_arguments)]
async fn run_orchestrate(
    g: &crate::cli::GlobalArgs,
    stdin: Option<String>,
    stdin_file: Option<String>,
    max_ms: u64,
    raw: bool,
    keep_daemon: bool,
    cwd: Option<String>,
    env: Vec<String>,
    argv: Vec<String>,
) -> Result<()> {
    use agent_tui_protocol::PaneId;
    use agent_tui_protocol::request::{StdinMode, WaitCondition};
    use std::time::{Duration, Instant};

    if argv.is_empty() {
        return Err(anyhow!("run requires at least one positional argv"));
    }
    // Interpret printf-style escapes in --stdin <text>: `\n`, `\r`,
    // `\t`, `\\`, `\0`, `\xNN`. Most CLIs need a trailing newline to
    // accept a prompt; making the user type `$'\n'` was a paper cut.
    let stdin_bytes: Option<Vec<u8>> = match (stdin, stdin_file) {
        (Some(t), None) => Some(interpret_escapes(&t)),
        (None, Some(p)) => Some(std::fs::read(&p).map_err(|e| anyhow!("read {p}: {e}"))?),
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!("clap conflicts_with"),
    };

    let layout = client::layout_for(&g.session, g.socket_dir.as_deref());
    let started = Instant::now();

    // 1. Spawn with pipe stdin (or closed if no input — saves a round
    //    trip).
    let stdin_mode = if stdin_bytes.is_some() {
        StdinMode::Pipe
    } else {
        StdinMode::Closed
    };
    let env_pairs = parse_env_pairs(&env)?;
    let spawn_env = client::one_shot(
        &layout,
        Command::Spawn {
            argv: argv.clone(),
            cwd,
            size: None,
            stdin: stdin_mode,
            env: env_pairs,
        },
    )
    .await?;
    if spawn_env.response.is_failure() {
        // Surface the error envelope to the caller.
        println!("{}", serde_json::to_string(&spawn_env)?);
        std::process::exit(2);
    }
    let pane = spawn_env
        .response
        .data
        .as_ref()
        .and_then(|d| d.get("pane"))
        .and_then(serde_json::Value::as_str)
        .map(|s| PaneId(s.to_string()));

    // 2. Optionally write stdin bytes + close-stdin.
    if let Some(bytes) = &stdin_bytes {
        let write_env = client::one_shot(
            &layout,
            Command::Stdin {
                pane: pane.clone(),
                bytes_hex: hex::encode(bytes),
            },
        )
        .await?;
        if write_env.response.is_failure() {
            // Best-effort cleanup before surfacing.
            let _ = client::one_shot(&layout, Command::Die { pane: pane.clone() }).await;
            println!("{}", serde_json::to_string(&write_env)?);
            std::process::exit(2);
        }
        let _ = client::one_shot(&layout, Command::CloseStdin { pane: pane.clone() }).await?;
    }

    // 3. Wait for the child to exit.
    let wait_env = client::one_shot(
        &layout,
        Command::Wait {
            pane: pane.clone(),
            condition: WaitCondition::Exit,
            timeout: Duration::from_millis(max_ms),
        },
    )
    .await?;
    let exit_code: Option<i64> = wait_env
        .response
        .data
        .as_ref()
        .and_then(|d| d.get("exit_code"))
        .and_then(serde_json::Value::as_i64);

    // 4. Tail the bytes the child wrote.
    let tail_env = client::one_shot(
        &layout,
        Command::Tail {
            pane: pane.clone(),
            since: 0,
            strip_ansi: !raw,
            follow: false,
        },
    )
    .await?;
    let output: serde_json::Value = if raw {
        tail_env
            .response
            .data
            .as_ref()
            .and_then(|d| d.get("bytes_b64"))
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    } else {
        tail_env
            .response
            .data
            .as_ref()
            .and_then(|d| d.get("text"))
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    };

    // 5. Cleanup: best-effort die on the pane.
    let _ = client::one_shot(&layout, Command::Die { pane: pane.clone() }).await;

    // 6. We considered auto-shutting the daemon down here, but it
    //    races with back-to-back `run` calls: while the shutdown
    //    propagates, the next `run`'s client sees a dying daemon and
    //    fails with "daemon closed without responding." The
    //    idle-timeout (5min default) is the right backstop instead.
    //    `--keep-daemon` is preserved as a no-op for forward-compat
    //    in case we add explicit shutdown back later.
    let _ = keep_daemon;

    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let output_key = if raw { "stdout_b64" } else { "stdout" };
    let payload = serde_json::json!({
        "argv": argv,
        "exit_code": exit_code,
        output_key: output,
        "elapsed_ms": elapsed_ms,
    });
    if g.json {
        println!("{}", serde_json::to_string(&payload)?);
    } else if raw {
        // Pretty-print: argv + exit + base64 length, dump base64.
        println!("argv: {}", argv.join(" "));
        println!(
            "exit_code: {}",
            exit_code.map_or("?".into(), |c| c.to_string())
        );
        println!("elapsed_ms: {elapsed_ms}");
        if let Some(s) = output.as_str() {
            println!("stdout_b64: {s}");
        }
    } else {
        // Default pretty: argv, exit, the text body verbatim.
        if let Some(s) = output.as_str() {
            print!("{s}");
            if !s.ends_with('\n') {
                println!();
            }
        }
        eprintln!(
            "[exit {}] [{} ms] [{}]",
            exit_code.map_or("?".into(), |c| c.to_string()),
            elapsed_ms,
            argv.join(" ")
        );
    }
    if exit_code.unwrap_or(1) != 0 {
        std::process::exit(exit_code.and_then(|c| c.try_into().ok()).unwrap_or(1));
    }
    Ok(())
}

/// Drive `tail --follow` — stream chunks from the daemon to stdout.
/// Each envelope is one chunk (or the terminal `{type: "eof"}`).
/// Under `--json`, prints each envelope as NDJSON; otherwise, prints
/// the raw bytes/text and exits silently on EOF.
async fn tail_follow(
    g: &crate::cli::GlobalArgs,
    pane: Option<String>,
    since: u64,
    strip_ansi: bool,
) -> Result<()> {
    use base64::Engine as _;
    use std::io::Write;
    let layout = client::layout_for(&g.session, g.socket_dir.as_deref());
    let cmd = Command::Tail {
        pane: pane.map(agent_tui_protocol::PaneId),
        since,
        strip_ansi,
        follow: true,
    };
    let mut stdout = std::io::stdout().lock();
    client::stream(&layout, cmd, |env| {
        if env.response.is_failure() {
            // Print the error envelope and stop.
            let line = serde_json::to_string(env)?;
            writeln!(stdout, "{line}").ok();
            return Ok(false);
        }
        if g.json {
            let line = serde_json::to_string(env)?;
            writeln!(stdout, "{line}").ok();
        }
        let Some(data) = env.response.data.as_ref() else {
            return Ok(true);
        };
        let ty = data.get("type").and_then(serde_json::Value::as_str);
        match ty {
            Some("chunk") if !g.json => {
                if strip_ansi {
                    if let Some(text) = data.get("text").and_then(serde_json::Value::as_str) {
                        stdout.write_all(text.as_bytes()).ok();
                        stdout.flush().ok();
                    }
                } else if let Some(b64) = data.get("bytes_b64").and_then(serde_json::Value::as_str)
                    && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64)
                {
                    stdout.write_all(&bytes).ok();
                    stdout.flush().ok();
                }
            }
            Some("eof") => return Ok(false),
            _ => {}
        }
        Ok(true)
    })
    .await
}

/// Replay an asciicast file through a fresh engine and emit the
/// resulting snapshot. Used for regression: locking in correctness
/// without re-spawning real children.
///
/// Cast format (asciicast v2/v3 — we accept either by reading lines):
///
/// ```text
/// {"version":2, "width":80, "height":24, ...}    ← optional header
/// [0.012, "o", "<bytes>"]
/// [0.015, "o", "<bytes>"]
/// ...
/// ```
///
/// We only read `o` events (output bytes the child wrote). `i`, `r`,
/// `m`, `s`, `g` events from the recorder are skipped for replay.
#[allow(clippy::unused_async)]
async fn replay_cast(
    g: &crate::cli::GlobalArgs,
    cast: String,
    expect: Option<String>,
    mode: crate::cli::SnapshotMode,
    cols: u16,
    rows: u16,
) -> Result<()> {
    use agent_tui_engine::Engine as _;
    use agent_tui_engine_alacritty::AlacrittyEngine;
    use std::io::{BufRead, BufReader};

    let file = std::fs::File::open(&cast).map_err(|e| anyhow!("open cast {cast}: {e}"))?;
    let engine = AlacrittyEngine::new(cols, rows);
    let mut events = 0u64;
    for line in BufReader::new(file).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || !line.starts_with('[') {
            // Header object on the first line; skip.
            continue;
        }
        let arr: Vec<serde_json::Value> = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // [timestamp, event_type, payload]
        let Some(ev_type) = arr.get(1).and_then(serde_json::Value::as_str) else {
            continue;
        };
        if ev_type != "o" {
            continue;
        }
        let Some(payload) = arr.get(2).and_then(serde_json::Value::as_str) else {
            continue;
        };
        events += 1;
        engine
            .feed(payload.as_bytes())
            .map_err(|e| anyhow!("engine.feed on event {events}: {e}"))?;
    }

    // Render snapshot. We can't go through the daemon (no PTY); call
    // the snapshot builder inline. Use a minimal stand-in that
    // matches the daemon's mode dispatch.
    let snap = engine.snapshot();
    let payload = render_replay_snapshot(&snap, mode);

    if g.json {
        println!("{}", serde_json::to_string(&payload)?);
    }

    let Some(expect_path) = expect else {
        if !g.json {
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        eprintln!("[replayed {events} event(s) in {mode:?} mode]");
        return Ok(());
    };

    let expected: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&expect_path).map_err(|e| anyhow!("read {expect_path}: {e}"))?,
    )?;
    if expected != payload {
        eprintln!("replay: SNAPSHOT MISMATCH");
        eprintln!("expected: {}", serde_json::to_string_pretty(&expected)?);
        eprintln!("actual:   {}", serde_json::to_string_pretty(&payload)?);
        std::process::exit(1);
    }
    eprintln!("replay: PASS  ({events} events)");
    Ok(())
}

/// Build a small snapshot payload for replay output. We can't reuse
/// the daemon's `build_snapshot` (it's behind handler-private types
/// and needs a Pane + adapter), so we render the modes we care about
/// directly here. Text and cells modes are easy; outline falls back
/// to the generic adapter shape via a tiny inline render.
fn render_replay_snapshot(
    snap: &agent_tui_engine::EngineSnapshot,
    mode: crate::cli::SnapshotMode,
) -> serde_json::Value {
    use crate::cli::SnapshotMode;
    let rows = usize::from(snap.grid.rows);
    let cols = usize::from(snap.grid.cols);
    let text = || -> String {
        let mut lines: Vec<String> = Vec::with_capacity(rows);
        for r in 0..rows {
            let mut line = String::with_capacity(cols);
            for c in 0..cols {
                let cell = &snap.grid.cells[r * cols + c];
                if cell.ch.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(&cell.ch);
                }
            }
            lines.push(line.trim_end().to_string());
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines.join("\n")
    };
    match mode {
        SnapshotMode::Text => serde_json::json!({
            "text": text(),
            "cols": cols,
            "rows": rows,
            "hash": snap.canonical_hash(),
            "sequence": snap.sequence,
        }),
        SnapshotMode::Outline
        | SnapshotMode::Adapter
        | SnapshotMode::Cells
        | SnapshotMode::Hybrid => {
            // Use text shape as the "lowest common denominator" for
            // replay comparison. Cell-level replay diffs are too
            // noisy; outline diffs would need an adapter that we
            // don't have in the offline replay context. The text
            // mode is the right level for regression coverage.
            serde_json::json!({
                "text": text(),
                "cols": cols,
                "rows": rows,
                "hash": snap.canonical_hash(),
                "sequence": snap.sequence,
            })
        }
    }
}

/// Emit a JSON snapshot of the CLI surface — every subcommand + flag
/// reachable from the top-level `Cli`. Consumed by `cargo xtask
/// cli-coverage` to check docs vs reality without parsing `--help`
/// text.
fn dump_surface() -> Result<()> {
    use clap::CommandFactory;

    fn walk(cmd: &clap::Command) -> serde_json::Value {
        let flags: Vec<String> = cmd
            .get_arguments()
            .filter_map(|a| {
                // Skip global-position positionals.
                a.get_long().map(|long| format!("--{long}"))
            })
            .collect();
        let subs: Vec<serde_json::Value> = cmd
            .get_subcommands()
            .filter(|s| s.get_name() != "help")
            .map(walk)
            .collect();
        serde_json::json!({
            "name": cmd.get_name(),
            "flags": flags,
            "subcommands": subs,
        })
    }

    let root = crate::cli::Cli::command();
    let payload = walk(&root);
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

/// `ask` — sugar over `run` driven by TOML recipes (bundled +
/// `~/.config/agent-tui/recipes/*.toml`). The recipe carries
/// argv + optional bash-cat wrapping + optional answer-extraction
/// markers + a default `--max`.
///
/// Adding a new provider is a TOML file in the user's recipe dir
/// (no rebuild). See `recipes.rs` for the schema.
async fn ask_sugar(
    g: &crate::cli::GlobalArgs,
    provider: &str,
    prompt: Vec<String>,
    max_ms_override: u64,
) -> Result<()> {
    let prompt_text = prompt.join(" ");
    if prompt_text.trim().is_empty() {
        return Err(anyhow!("ask requires a non-empty prompt"));
    }
    let reg = crate::recipes::RecipeRegistry::load();
    let recipe = reg.get(provider).ok_or_else(|| {
        anyhow!(
            "no recipe for provider `{provider}` — known: {}",
            reg.known().join(", ")
        )
    })?;
    // Use the recipe's default_max_ms when caller didn't override
    // (clap default is 120000; if it's still that AND the recipe
    // suggests something different, prefer the recipe).
    let max_ms = if max_ms_override == 120_000 {
        recipe.default_max_ms.unwrap_or(max_ms_override)
    } else {
        max_ms_override
    };
    let argv = recipe.effective_argv();
    // Capture the raw output, then apply the recipe's extractor.
    let raw = run_capture(g, Some(prompt_text.clone()), max_ms, argv).await?;
    let answer = recipe.extract(&raw);
    print!("{answer}");
    if !answer.ends_with('\n') {
        println!();
    }
    Ok(())
}

/// Like `run_orchestrate` but returns the captured stdout instead of
/// printing it. Used by `ask` so the recipe can apply its
/// answer-extraction before we emit anything to stdout.
async fn run_capture(
    g: &crate::cli::GlobalArgs,
    stdin: Option<String>,
    max_ms: u64,
    argv: Vec<String>,
) -> Result<String> {
    use agent_tui_protocol::PaneId;
    use agent_tui_protocol::request::{StdinMode, WaitCondition};
    use std::time::Duration;

    let stdin_bytes: Option<Vec<u8>> = stdin.map(|s| interpret_escapes(&s));
    let layout = client::layout_for(&g.session, g.socket_dir.as_deref());

    let stdin_mode = if stdin_bytes.is_some() {
        StdinMode::Pipe
    } else {
        StdinMode::Closed
    };
    let spawn_env = client::one_shot(
        &layout,
        Command::Spawn {
            argv: argv.clone(),
            cwd: None,
            size: None,
            stdin: stdin_mode,
            env: Vec::new(),
        },
    )
    .await?;
    if spawn_env.response.is_failure() {
        anyhow::bail!(
            "spawn failed: {}",
            spawn_env
                .response
                .error
                .as_ref()
                .map_or("(no error body)", |e| e.message.as_str())
        );
    }
    let pane = spawn_env
        .response
        .data
        .as_ref()
        .and_then(|d| d.get("pane"))
        .and_then(serde_json::Value::as_str)
        .map(|s| PaneId(s.to_string()));

    if let Some(bytes) = &stdin_bytes {
        let _ = client::one_shot(
            &layout,
            Command::Stdin {
                pane: pane.clone(),
                bytes_hex: hex::encode(bytes),
            },
        )
        .await?;
        let _ = client::one_shot(&layout, Command::CloseStdin { pane: pane.clone() }).await?;
    }
    let _ = client::one_shot(
        &layout,
        Command::Wait {
            pane: pane.clone(),
            condition: WaitCondition::Exit,
            timeout: Duration::from_millis(max_ms),
        },
    )
    .await?;
    let tail_env = client::one_shot(
        &layout,
        Command::Tail {
            pane: pane.clone(),
            since: 0,
            strip_ansi: true,
            follow: false,
        },
    )
    .await?;
    let text = tail_env
        .response
        .data
        .as_ref()
        .and_then(|d| d.get("text"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let _ = client::one_shot(&layout, Command::Die { pane }).await;
    Ok(text)
}

/// `edit` — open `path` in `$EDITOR` (default vim), wait for the editor
/// to exit, return the file's content.
#[allow(clippy::unused_async)]
async fn edit_sugar(
    g: &crate::cli::GlobalArgs,
    path: String,
    editor_override: Option<String>,
) -> Result<()> {
    let editor = editor_override
        .or_else(|| std::env::var("EDITOR").ok())
        .unwrap_or_else(|| "vim".into());
    let argv: Vec<String> = vec![editor, path.clone()];
    // No stdin; child gets a PTY. Block until the editor exits, then
    // print the file's content.
    run_orchestrate(g, None, None, 600_000, false, false, None, Vec::new(), argv).await?;
    let content =
        std::fs::read_to_string(&path).map_err(|e| anyhow!("read {path} after edit: {e}"))?;
    print!("{content}");
    Ok(())
}

/// `watch` — spawn + tail --follow until exit. Streams the child's
/// stdout to ours in real time.
async fn watch_sugar(g: &crate::cli::GlobalArgs, argv: Vec<String>) -> Result<()> {
    if argv.is_empty() {
        return Err(anyhow!("watch requires at least one positional argv"));
    }
    let layout = client::layout_for(&g.session, g.socket_dir.as_deref());
    let spawn_env = client::one_shot(
        &layout,
        Command::Spawn {
            argv: argv.clone(),
            cwd: None,
            size: None,
            stdin: agent_tui_protocol::request::StdinMode::Pty,
            env: Vec::new(),
        },
    )
    .await?;
    if spawn_env.response.is_failure() {
        println!("{}", serde_json::to_string(&spawn_env)?);
        std::process::exit(2);
    }
    let pane = spawn_env
        .response
        .data
        .as_ref()
        .and_then(|d| d.get("pane"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    tail_follow(g, pane.clone(), 0, true).await?;
    // Best-effort die when streaming ends.
    let _ = client::one_shot(
        &layout,
        Command::Die {
            pane: pane.map(agent_tui_protocol::PaneId),
        },
    )
    .await;
    Ok(())
}

/// Parse repeated `--env K=V` strings into `(K, V)` tuples for the
/// wire Spawn command. Rejects entries without an `=` (we don't try
/// to be clever about `--env JUSTAKEY` because the intent is unclear
/// — empty value or "inherit"? Let the caller decide and write `K=`).
fn parse_env_pairs(spec: &[String]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::with_capacity(spec.len());
    for s in spec {
        let Some((k, v)) = s.split_once('=') else {
            return Err(anyhow!(
                "--env requires `K=V`, got `{s}` (use `K=` for an empty value)"
            ));
        };
        if k.is_empty() {
            return Err(anyhow!("--env key cannot be empty: `{s}`"));
        }
        out.push((k.to_string(), v.to_string()));
    }
    Ok(out)
}

/// Interpret a small set of C/printf escapes in a string argument:
/// `\n`, `\r`, `\t`, `\0`, `\\`, `\"`. Unknown escapes are left
/// literal — agents passing `\latex` shouldn't lose the backslash.
fn interpret_escapes(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            // Push the char as UTF-8.
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.peek().copied() {
            Some('n') => {
                chars.next();
                out.push(b'\n');
            }
            Some('r') => {
                chars.next();
                out.push(b'\r');
            }
            Some('t') => {
                chars.next();
                out.push(b'\t');
            }
            Some('0') => {
                chars.next();
                out.push(0);
            }
            Some('\\') => {
                chars.next();
                out.push(b'\\');
            }
            Some('"') => {
                chars.next();
                out.push(b'"');
            }
            // Unknown escape — preserve the backslash literally.
            _ => out.push(b'\\'),
        }
    }
    out
}

async fn one_shot_print(g: &crate::cli::GlobalArgs, cmd: Command) -> Result<()> {
    let layout = client::layout_for(&g.session, g.socket_dir.as_deref());
    let env = client::one_shot(&layout, cmd).await?;
    let out = serde_json::to_string(&env)?;
    println!("{out}");
    // Exit code: zero on success, non-zero on protocol failure.
    if env.response.is_failure() {
        std::process::exit(2);
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn cli_command_to_protocol(cmd: CliCmd) -> Result<Command> {
    match cmd {
        CliCmd::Spawn {
            argv,
            stdin,
            cwd,
            env,
        } => Ok(Command::Spawn {
            argv,
            cwd,
            size: None,
            stdin: stdin.into(),
            env: parse_env_pairs(&env)?,
        }),
        CliCmd::List { all } => Ok(Command::List { all }),
        CliCmd::Snapshot {
            pane,
            mode,
            png,
            annotate,
            select,
            all,
            keep_color,
        } => Ok(Command::Snapshot {
            pane: pane.map(agent_tui_protocol::PaneId),
            mode: mode.into(),
            png: png.map(|p| p.to_string_lossy().into_owned()),
            annotate,
            select,
            all,
            keep_color,
        }),
        CliCmd::Press { pane, keys, to } => Ok(Command::Press {
            pane: pane.map(agent_tui_protocol::PaneId),
            keys,
            to,
        }),
        CliCmd::Type { pane, text, to } => Ok(Command::Type {
            pane: pane.map(agent_tui_protocol::PaneId),
            text,
            to,
        }),
        CliCmd::SendAnsi { pane, bytes_hex } => Ok(Command::SendAnsi {
            pane: pane.map(agent_tui_protocol::PaneId),
            bytes_hex,
        }),
        CliCmd::Stdin {
            pane,
            text,
            bytes_hex,
        } => {
            let bytes_hex = match (text, bytes_hex) {
                (Some(t), None) => hex::encode(t.as_bytes()),
                (None, Some(h)) => h,
                (None, None) => {
                    anyhow::bail!("stdin requires either --text or --bytes-hex")
                }
                (Some(_), Some(_)) => unreachable!("clap conflicts_with"),
            };
            Ok(Command::Stdin {
                pane: pane.map(agent_tui_protocol::PaneId),
                bytes_hex,
            })
        }
        CliCmd::CloseStdin { pane } => Ok(Command::CloseStdin {
            pane: pane.map(agent_tui_protocol::PaneId),
        }),
        CliCmd::Tail {
            pane,
            since,
            strip_ansi,
            follow,
        } => Ok(Command::Tail {
            pane: pane.map(agent_tui_protocol::PaneId),
            since,
            strip_ansi,
            follow,
        }),
        CliCmd::Resize { pane, cols, rows } => Ok(Command::Resize {
            pane: pane.map(agent_tui_protocol::PaneId),
            cols,
            rows,
        }),
        CliCmd::Signal { pane, signal } => Ok(Command::Signal {
            pane: pane.map(agent_tui_protocol::PaneId),
            signal,
        }),
        CliCmd::Die { pane } => Ok(Command::Die {
            pane: pane.map(agent_tui_protocol::PaneId),
        }),
        CliCmd::Pane(p) => match p.action {
            PaneAction::Focus { pane } => Ok(Command::Focus {
                pane: if pane.eq_ignore_ascii_case("none") {
                    None
                } else {
                    Some(agent_tui_protocol::PaneId(pane))
                },
            }),
        },
        CliCmd::Wait(a) => Ok(Command::Wait {
            pane: a.pane.clone().map(agent_tui_protocol::PaneId),
            condition: wait_condition_from_args(&a)?,
            timeout: std::time::Duration::from_millis(a.max),
        }),
        CliCmd::Eval {
            pane,
            adapter,
            expr,
        } => Ok(Command::Eval {
            pane: pane.map(agent_tui_protocol::PaneId),
            adapter,
            expr,
        }),
        // Daemon, Doctor, Skills, Mcp are handled above.
        _ => Err(anyhow!("internal: dispatch wired the wrong command")),
    }
}

fn wait_condition_from_args(
    a: &crate::cli::WaitArgs,
) -> Result<agent_tui_protocol::request::WaitCondition> {
    use agent_tui_protocol::request::WaitCondition;
    if let Some(seq) = a.since {
        return Ok(WaitCondition::Since { since: seq });
    }
    if let Some(h) = a.hash.clone() {
        return Ok(WaitCondition::Hash { hash: h });
    }
    if let Some(ms) = a.idle {
        return Ok(WaitCondition::Idle { quiet_ms: ms });
    }
    if let Some(r) = a.text.clone() {
        return Ok(WaitCondition::Text { regex: r });
    }
    if let Some(ms) = a.cursor_stable {
        return Ok(WaitCondition::CursorStable { stable_ms: ms });
    }
    if let Some(on) = a.alt_screen {
        return Ok(WaitCondition::AltScreen { on });
    }
    if a.exit {
        return Ok(WaitCondition::Exit);
    }
    if let Some(sel) = a.ref_selector.clone() {
        return Ok(WaitCondition::Ref {
            selector: sel,
            gone: a.gone,
        });
    }
    Err(anyhow!("wait requires exactly one mode flag"))
}

/// `doctor` probes the daemon (`DaemonStatus`) for reachability + version +
/// pane count and surfaces a CLI-side health report. `--fix` and
/// `--diagnostic-bundle` still surface as fields but the destructive /
/// archival paths land in P1.
async fn doctor(g: &crate::cli::GlobalArgs, args: &crate::cli::DoctorArgs) -> Result<()> {
    let layout = client::layout_for(&g.session, g.socket_dir.as_deref());

    let mut report = serde_json::json!({
        "ok": true,
        "cli_version": env!("CARGO_PKG_VERSION"),
        "session": g.session,
        "socket": layout.socket.display().to_string(),
        "quick": args.quick,
        "fix": args.fix,
        "diagnostic_bundle": args.diagnostic_bundle.as_ref().map(|p| p.display().to_string()),
    });

    match client::one_shot(&layout, Command::DaemonStatus).await {
        Ok(env) if env.response.success => {
            report["daemon"] = serde_json::json!({
                "reachable": true,
                "version": env.version,
                "protocol": env.protocol,
                "status": env.response.data,
            });
        }
        Ok(env) => {
            report["ok"] = serde_json::json!(false);
            report["daemon"] = serde_json::json!({
                "reachable": true,
                "error": env.response.error,
            });
        }
        Err(e) => {
            report["ok"] = serde_json::json!(false);
            report["daemon"] = serde_json::json!({
                "reachable": false,
                "error": format!("{e:#}"),
            });
        }
    }

    println!("{report}");
    Ok(())
}

/// `session gc` — prune dead sessions' on-disk state.
async fn session_gc(
    g: &crate::cli::GlobalArgs,
    older_than_days: u64,
    all: bool,
    dry_run: bool,
) -> Result<()> {
    // The socket root is session-independent — every session's sidecars
    // live in the same directory — so any session name resolves it.
    let socket_root = client::layout_for(&g.session, g.socket_dir.as_deref()).root;
    let state_root = agent_tui_daemon::paths::state_root();
    let opts = gc::GcOptions {
        older_than: std::time::Duration::from_secs(older_than_days.saturating_mul(86_400)),
        prune_all: all,
        dry_run,
    };
    let report = gc::run_gc(
        &socket_root,
        state_root.as_deref(),
        &opts,
        std::time::SystemTime::now(),
    )
    .await?;

    if g.json {
        println!(
            "{}",
            serde_json::json!({
                "pruned": report.pruned,
                "skipped_alive": report.skipped_alive,
                "skipped_young": report.skipped_young,
                "dry_run": report.dry_run,
            })
        );
    } else {
        let verb = if report.dry_run {
            "would prune"
        } else {
            "pruned"
        };
        if report.pruned.is_empty() {
            println!("{verb}: 0 sessions");
        } else {
            println!("{verb}: {}", report.pruned.join(", "));
        }
        eprintln!(
            "[{} pruned] [{} alive, kept] [{} too young, kept]",
            report.pruned.len(),
            report.skipped_alive,
            report.skipped_young,
        );
    }
    Ok(())
}

fn skills(args: &crate::cli::SkillsArgs) -> Result<()> {
    use crate::skills as sk;
    match &args.action {
        crate::cli::SkillsAction::List => {
            // Two-column NAME / DESCRIPTION layout, padded to the
            // widest name so output stays aligned even as more skills
            // land.
            let widest = sk::ALL_SKILLS
                .iter()
                .map(|s| s.name.len())
                .max()
                .unwrap_or(0);
            for s in sk::ALL_SKILLS {
                println!(
                    "{name:width$}  {desc}",
                    name = s.name,
                    width = widest,
                    desc = s.description.trim()
                );
            }
        }
        crate::cli::SkillsAction::Get { name, full } => {
            let Some(skill) = sk::find(name) else {
                anyhow::bail!(
                    "unknown skill `{name}`. Run `agent-tui skills list` to see what's available."
                );
            };
            print!("{}", sk::render(skill, *full));
        }
    }
    // Use the engine kind so we don't get unused-variant warnings before the
    // engine selection is wired through to the daemon's actual choice.
    let _ = EngineKind::Wezterm;
    Ok(())
}
