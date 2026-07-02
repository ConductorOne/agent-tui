//! CLI-side client that opens the daemon's Unix socket and round-trips
//! one JSON request → one JSON response.
//!
//! This is the only path the CLI uses to reach the daemon. When no socket is
//! found we lazily spawn `agent-tui daemon run --session <s>` and retry with
//! a short backoff — mirrors the agent-browser bootstrap.

use std::io::ErrorKind;
use std::path::Path;
#[cfg(not(windows))]
use std::process::Stdio;
use std::time::Duration;

use agent_tui_daemon::SocketLayout;
use agent_tui_protocol::{Command, PROTOCOL_VERSION, Request, ResponseEnvelope, SessionId};
use anyhow::{Context, Result, bail};
use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::traits::tokio::Stream as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;
use tracing::debug;
use uuid::Uuid;

/// Policy values the client must copy into a daemon it lazily starts.
///
/// Foreground `daemon run` receives these from clap directly. Lazy-spawn is
/// different: the parent CLI command already parsed the globals, then starts a
/// fresh `agent-tui daemon run` child. Keep the policy payload explicit so the
/// spawned daemon cannot silently fall back to permissive defaults.
#[derive(Debug, Clone, Default)]
pub struct LazySpawnConfig {
    pub allowed_binaries: Option<String>,
}

impl LazySpawnConfig {
    #[must_use]
    pub fn from_allowed_binaries(allowed_binaries: Option<&str>) -> Self {
        Self {
            allowed_binaries: allowed_binaries.map(str::to_string),
        }
    }

    fn resolved_allowed_binaries(&self) -> Option<String> {
        self.allowed_binaries
            .clone()
            .or_else(|| std::env::var("AGENT_TUI_ALLOWED_BINARIES").ok())
    }
}

/// Connect, send one request, read one response. Spawns the daemon if no
/// socket is listening.
pub async fn one_shot(layout: &SocketLayout, command: Command) -> Result<ResponseEnvelope> {
    one_shot_with_config(layout, command, &LazySpawnConfig::default()).await
}

/// [`one_shot`] with explicit lazy-spawn policy propagation.
pub async fn one_shot_with_config(
    layout: &SocketLayout,
    command: Command,
    lazy_spawn: &LazySpawnConfig,
) -> Result<ResponseEnvelope> {
    let stream = match connect(layout).await {
        Ok(s) => s,
        Err(e) if is_unreachable(&e) => {
            debug!(socket = %layout.socket.display(), "daemon unreachable, spawning");
            spawn_daemon(layout, lazy_spawn)?;
            wait_for_socket(layout, Duration::from_secs(3)).await?
        }
        Err(e) => return Err(e),
    };
    send_and_recv(stream, command).await
}

async fn connect(layout: &SocketLayout) -> Result<Stream> {
    let name = agent_tui_daemon::paths::socket_name(layout)
        .with_context(|| format!("build socket name for {}", layout.socket.display()))?;
    Stream::connect(name)
        .await
        .with_context(|| format!("connect to daemon socket {}", layout.socket.display()))
}

/// Non-spawning liveness check: is a daemon answering this session's
/// socket? Unlike [`one_shot`], this never lazily spawns a daemon — a
/// failed connect (missing socket file or a stale socket with no
/// listener) reports `false`. Used by `session gc` to avoid reaping a
/// live session.
pub(crate) async fn probe_live(layout: &SocketLayout) -> bool {
    connect(layout).await.is_ok()
}

fn is_unreachable(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|e| e.downcast_ref::<std::io::Error>())
        .any(|io| {
            matches!(
                io.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            )
        })
}

fn spawn_daemon(layout: &SocketLayout, lazy_spawn: &LazySpawnConfig) -> Result<()> {
    let exe = std::env::current_exe().context("current_exe")?;
    let session = layout
        .socket
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default")
        .to_string();

    // Shared argv. Globals (`--session`, `--socket-dir`) precede the subcommand;
    // `--monitor-parent` is a `daemon run` flag.
    let mut args: Vec<std::ffi::OsString> = vec![
        "--session".into(),
        session.into(),
        "--socket-dir".into(),
        layout.root.clone().into_os_string(),
        "daemon".into(),
        "run".into(),
    ];
    // Opt-in parent-monitor. The CLI process is ephemeral — using *our* PID
    // would shut the daemon down the instant the CLI's one-shot RPC finishes,
    // breaking the whole "daemon outlives CLI" design. Tests (and any other
    // long-lived parent) opt into the cleanup behavior by setting
    // `AGENT_TUI_MONITOR_PARENT_PID` to a PID they control before the lazy-spawn.
    if let Ok(pid) = std::env::var("AGENT_TUI_MONITOR_PARENT_PID") {
        args.push("--monitor-parent".into());
        args.push(pid.into());
    }

    // Forward governance settings to the lazily-spawned daemon child. The CLI's
    // `--allowed-binaries` value lives on the *parent* invocation; copy the
    // parsed value explicitly, falling back to the env-var binding clap declares
    // for callers that configured policy through the environment.
    let mut env_overrides: Vec<(std::ffi::OsString, std::ffi::OsString)> = Vec::new();
    if let Some(csv) = lazy_spawn.resolved_allowed_binaries() {
        env_overrides.push((
            std::ffi::OsString::from("AGENT_TUI_ALLOWED_BINARIES"),
            std::ffi::OsString::from(csv),
        ));
    }

    // On Windows, spawn the DETACHED daemon so it inherits ONLY three fresh NUL
    // std handles — nothing else. A plain `Command::spawn` leaves
    // `bInheritHandles = TRUE`; inside a `cmd.exe` pipeline (`run … | consumer`)
    // that lets the long-lived daemon inherit and pin open a pipe handle cmd
    // leaves in this process, so the consumer never sees EOF (a real hang).
    // Stripping only our own std handles is insufficient — see
    // `agent_tui_daemon::win_spawn`. The empty-but-for-NUL inherit allow-list
    // also gives the daemon a fresh process group + no console (DETACHED), the
    // Windows analog of the Unix daemon landing in its own process group.
    #[cfg(windows)]
    {
        agent_tui_daemon::win_spawn::spawn_detached_no_inherit(&exe, &args, &env_overrides)
            .context("spawn daemon")
    }
    // Elsewhere: redirect the daemon's stdio to null and spawn. The null `dup2`
    // closes the client's fd in the child, so the pipe-inheritance leak the
    // Windows path guards against cannot happen here.
    #[cfg(not(windows))]
    {
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.args(&args);
        for (k, v) in &env_overrides {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _child = cmd.spawn().context("spawn daemon")?;
        Ok(())
    }
}

async fn wait_for_socket(layout: &SocketLayout, max_wait: Duration) -> Result<Stream> {
    let deadline = tokio::time::Instant::now() + max_wait;
    loop {
        let name = agent_tui_daemon::paths::socket_name(layout)?;
        match Stream::connect(name).await {
            Ok(s) => return Ok(s),
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(e) => bail!(
                "daemon did not bind {} within {}ms: {e}",
                layout.socket.display(),
                max_wait.as_millis()
            ),
        }
    }
}

/// Streaming variant of [`one_shot`]: send the command, then invoke
/// `on_envelope` for every `ResponseEnvelope` the daemon emits until
/// the connection closes or the envelope's `data.type == "eof"`.
/// Used by `tail --follow`.
pub async fn stream(
    layout: &SocketLayout,
    command: Command,
    on_envelope: impl FnMut(&ResponseEnvelope) -> Result<bool>,
) -> Result<()> {
    stream_with_config(layout, command, &LazySpawnConfig::default(), on_envelope).await
}

/// [`stream`] with explicit lazy-spawn policy propagation.
pub async fn stream_with_config(
    layout: &SocketLayout,
    command: Command,
    lazy_spawn: &LazySpawnConfig,
    mut on_envelope: impl FnMut(&ResponseEnvelope) -> Result<bool>,
) -> Result<()> {
    let stream = match connect(layout).await {
        Ok(s) => s,
        Err(e) if is_unreachable(&e) => {
            debug!(socket = %layout.socket.display(), "daemon unreachable, spawning");
            spawn_daemon(layout, lazy_spawn)?;
            wait_for_socket(layout, Duration::from_secs(3)).await?
        }
        Err(e) => return Err(e),
    };
    let req = Request {
        id: Uuid::new_v4(),
        protocol: PROTOCOL_VERSION,
        command,
    };
    let mut bytes = serde_json::to_vec(&req)?;
    bytes.push(b'\n');

    let (reader, mut writer) = tokio::io::split(stream);
    writer.write_all(&bytes).await?;

    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let env: ResponseEnvelope = serde_json::from_str(&line)
            .with_context(|| format!("parse streaming envelope: {line}"))?;
        let keep_going = on_envelope(&env)?;
        if !keep_going {
            return Ok(());
        }
    }
    Ok(())
}

/// Baseline read timeout for fast round-trip commands (spawn, snapshot,
/// stdin, …). These return promptly; if the daemon goes silent for this
/// long it is wedged, not working.
const BASE_READ_TIMEOUT: Duration = Duration::from_secs(25);

/// Safety margin added on top of a blocking command's own deadline. The
/// client read timeout must outlive the daemon's enforced deadline, or a
/// long-but-healthy wait fails spuriously on the client side.
const WAIT_TIMEOUT_MARGIN: Duration = Duration::from_secs(5);

/// How long the client should wait for the daemon's reply to a given
/// command. Blocking commands (`wait`, and the `wait --exit` step inside
/// `run`/`ask`) carry their own deadline via `--max`; the client must wait
/// at least that long plus a margin. A fixed cap here is the bug behind
/// `ask … --max 120000` dying at 25s while the daemon is still working.
fn read_timeout_for(command: &Command) -> Duration {
    match command {
        Command::Wait { timeout, .. } => timeout.saturating_add(WAIT_TIMEOUT_MARGIN),
        // `die --grace` blocks the daemon for up to the grace window while it
        // polls for group exit before escalating to SIGKILL; give the reply
        // room past that deadline.
        Command::Die {
            grace: Some(grace), ..
        } => grace.saturating_add(WAIT_TIMEOUT_MARGIN),
        _ => BASE_READ_TIMEOUT,
    }
}

async fn send_and_recv(stream: Stream, command: Command) -> Result<ResponseEnvelope> {
    let read_timeout = read_timeout_for(&command);
    let req = Request {
        id: Uuid::new_v4(),
        protocol: PROTOCOL_VERSION,
        command,
    };
    let mut bytes = serde_json::to_vec(&req)?;
    bytes.push(b'\n');

    let (reader, mut writer) = tokio::io::split(stream);
    writer.write_all(&bytes).await?;
    let mut lines = BufReader::new(reader).lines();
    let line = timeout(read_timeout, lines.next_line())
        .await
        .with_context(|| format!("daemon read timed out ({}s)", read_timeout.as_secs()))?
        .context("daemon read failed")?
        .context("daemon closed without responding")?;

    Ok(serde_json::from_str(&line)?)
}

/// Compute the `SocketLayout`, honoring an explicit override.
#[must_use]
pub fn layout_for(session: &str, override_root: Option<&Path>) -> SocketLayout {
    let session_id = SessionId(session.to_string());
    match override_root {
        Some(root) => SocketLayout::for_session_in(&session_id, root.to_path_buf()),
        None => SocketLayout::for_session(&session_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_tui_protocol::request::WaitCondition;

    #[test]
    fn wait_read_timeout_outlives_its_own_deadline() {
        // The regression: `ask … --max 120000` decomposes into a
        // `wait --exit` with a 120s deadline. The client must wait
        // longer than that, not cap at the old hardcoded 25s.
        let cmd = Command::Wait {
            pane: None,
            condition: WaitCondition::Exit,
            timeout: Duration::from_secs(120),
        };
        let got = read_timeout_for(&cmd);
        assert!(
            got > Duration::from_secs(120),
            "wait read timeout {got:?} must exceed its own 120s deadline"
        );
        assert_eq!(got, Duration::from_secs(120) + WAIT_TIMEOUT_MARGIN);
    }

    #[test]
    fn short_wait_still_gets_margin() {
        let cmd = Command::Wait {
            pane: None,
            condition: WaitCondition::Exit,
            timeout: Duration::from_secs(1),
        };
        assert_eq!(
            read_timeout_for(&cmd),
            Duration::from_secs(1) + WAIT_TIMEOUT_MARGIN
        );
    }

    #[test]
    fn non_wait_commands_use_base_timeout() {
        assert_eq!(
            read_timeout_for(&Command::Die {
                pane: None,
                grace: None
            }),
            BASE_READ_TIMEOUT
        );
        assert_eq!(
            read_timeout_for(&Command::List { all: false }),
            BASE_READ_TIMEOUT
        );
    }

    #[test]
    fn die_grace_extends_read_timeout() {
        // A graceful teardown blocks the daemon for up to the grace window;
        // the client read timeout must outlive it (mirrors the `wait` rule).
        let grace = Duration::from_secs(30);
        assert_eq!(
            read_timeout_for(&Command::Die {
                pane: None,
                grace: Some(grace),
            }),
            grace + WAIT_TIMEOUT_MARGIN
        );
    }

    #[test]
    fn enormous_wait_does_not_overflow() {
        let cmd = Command::Wait {
            pane: None,
            condition: WaitCondition::Exit,
            timeout: Duration::MAX,
        };
        // saturating_add must not panic.
        assert_eq!(read_timeout_for(&cmd), Duration::MAX);
    }
}
