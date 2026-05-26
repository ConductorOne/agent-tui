//! Subcommand dispatch — translates the clap-derived [`Cli`] into either a
//! daemon-side action (`daemon run`) or a one-shot client RPC against the
//! local daemon.

use agent_tui_daemon::{DaemonConfig, run_daemon};
use agent_tui_protocol::{Command, SessionId};
use anyhow::{Result, anyhow};

use crate::cli::{Cli, Command as CliCmd, DaemonAction, EngineKind, PaneAction};
use crate::client;

/// Top-level dispatch entry point.
pub async fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        CliCmd::Daemon(args) => match args.action {
            DaemonAction::Run => run_foreground_daemon(&cli.globals).await,
            DaemonAction::Status => one_shot_print(&cli.globals, Command::DaemonStatus).await,
            DaemonAction::Shutdown { force } => {
                one_shot_print(&cli.globals, Command::DaemonShutdown { force }).await
            }
        },
        CliCmd::Doctor(args) => doctor(&cli.globals, &args).await,
        CliCmd::Skills(args) => skills(&args),
        CliCmd::Mcp(_) => Err(anyhow!(
            "mcp serve not yet implemented (P4); track docs/RFC.md §13.4"
        )),
        // Everything else is a one-shot client RPC. The daemon currently
        // returns a friendly INTERNAL error for unwired ops; the CLI surfaces
        // that as a non-zero exit so callers can branch.
        other => {
            let cmd = cli_command_to_protocol(other)?;
            one_shot_print(&cli.globals, cmd).await
        }
    }
}

async fn run_foreground_daemon(g: &crate::cli::GlobalArgs) -> Result<()> {
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
    };
    let handle = run_daemon(cfg).await?;
    // Block until the daemon's shutdown notify fires. In v0.1.0 nothing
    // sets this signal — `daemon shutdown` doesn't yet hook back; you
    // need Ctrl-C.
    handle.shutdown.notified().await;
    Ok(())
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

fn cli_command_to_protocol(cmd: CliCmd) -> Result<Command> {
    match cmd {
        CliCmd::Spawn { argv } => Ok(Command::Spawn {
            argv,
            cwd: None,
            size: None,
        }),
        CliCmd::List { all } => Ok(Command::List { all }),
        CliCmd::Snapshot {
            pane,
            mode,
            png,
            annotate,
        } => Ok(Command::Snapshot {
            pane: pane.map(agent_tui_protocol::PaneId),
            mode: mode.into(),
            png: png.map(|p| p.to_string_lossy().into_owned()),
            annotate,
        }),
        CliCmd::Press { pane, keys } => Ok(Command::Press {
            pane: pane.map(agent_tui_protocol::PaneId),
            keys,
        }),
        CliCmd::Type { pane, text } => Ok(Command::Type {
            pane: pane.map(agent_tui_protocol::PaneId),
            text,
        }),
        CliCmd::SendAnsi { pane, bytes_hex } => Ok(Command::SendAnsi {
            pane: pane.map(agent_tui_protocol::PaneId),
            bytes_hex,
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

// Same rationale as `doctor`: real skill-loading from embedded data lands
// in P4 and will surface read errors.
#[allow(clippy::unnecessary_wraps)]
fn skills(args: &crate::cli::SkillsArgs) -> Result<()> {
    match &args.action {
        crate::cli::SkillsAction::List => {
            println!("core   — TBD: lands with skill-data embedding (P4)");
        }
        crate::cli::SkillsAction::Get { name, full } => {
            println!(
                "(skill `{name}`, full={full}: skill-data embedding lands in P4 per docs/RFC.md §16)"
            );
        }
    }
    // Use the engine kind so we don't get unused-variant warnings before the
    // engine selection is wired through to the daemon's actual choice.
    let _ = EngineKind::Wezterm;
    Ok(())
}
