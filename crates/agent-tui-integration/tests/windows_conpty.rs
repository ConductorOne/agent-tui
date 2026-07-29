//! Windows ConPTY end-to-end coverage for the parity work shipped in PR #124.
//!
//! These tests are `#[cfg(windows)]` with NO Cargo-feature gate, so the stock
//! `cargo test --workspace` CI leg on `windows-latest` runs them (the
//! docker/bwrap suites are feature-gated and stay Linux-only). Each test gets
//! an isolated session + socket dir, so the file is safe under cargo's
//! default parallel test threads, and every rig ties its daemon's lifetime to
//! the test process (`AGENT_TUI_MONITOR_PARENT_PID`) plus a best-effort
//! `daemon shutdown` on drop.
//!
//! Covered (each maps to a fix in #124):
//!  1. `run | consumer` in a `cmd.exe` pipeline sees prompt EOF (the `win_spawn`
//!     handle-inheritance allow-list).
//!  2. An interactive pane's startup DSR (`ESC[6n`) is answered, unblocking
//!     the child (engine `take_pty_writes` write-back via `dsr_probe`).
//!  3. `tail --follow` delivers a fast-exiting child's trailing bytes before
//!     the stream ends (`exit_drained` / `DRAIN_GRACE`).
//!  4. `die` reaps the whole descendant tree (`taskkill /F /T`).
//!  5. `signal SIGINT` interrupts via ETX written to the ConPTY input.
//!  6. `signal SIGBREAK` is rejected with an actionable error (no false
//!     success).

#![cfg(windows)]

use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use agent_tui_integration::{agent_tui_binary, workspace_root};
use anyhow::{Context, Result, bail};

/// Isolated per-test daemon context: unique session, private socket dir and
/// state home under %TEMP%. Drop shuts the daemon down best-effort and
/// removes the directory.
struct Rig {
    session: String,
    root: PathBuf,
    socket_dir: PathBuf,
    state_home: PathBuf,
}

impl Rig {
    fn new(name: &str) -> Result<Self> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let root = std::env::temp_dir().join(format!("agent-tui-win-e2e-{name}-{}", &id[..8]));
        let socket_dir = root.join("sock");
        let state_home = root.join("state");
        std::fs::create_dir_all(&socket_dir)
            .with_context(|| format!("create {}", socket_dir.display()))?;
        std::fs::create_dir_all(&state_home)
            .with_context(|| format!("create {}", state_home.display()))?;
        Ok(Self {
            session: format!("e2e-{}", &id[..8]),
            root,
            socket_dir,
            state_home,
        })
    }

    /// Per-rig env every child process needs (CLI invocations and the
    /// cmd.exe pipeline repro alike).
    fn apply_env(&self, cmd: &mut tokio::process::Command) {
        cmd.env("AGENT_TUI_SOCKET_DIR", &self.socket_dir)
            .env("AGENT_TUI_ALLOWED_BINARIES", "*")
            // Tie any lazy-spawned daemon's lifetime to this test process so
            // a panicking or SIGKILLed test runner doesn't orphan a daemon.
            .env(
                "AGENT_TUI_MONITOR_PARENT_PID",
                std::process::id().to_string(),
            )
            .env("XDG_STATE_HOME", &self.state_home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    }

    /// Base command with the per-rig env every invocation needs. `json`
    /// selects the global `--json` flag; text surfaces (`tail`, streaming
    /// verbs) are asserted on raw stdout instead.
    fn cli(&self, json: bool) -> tokio::process::Command {
        let bin = agent_tui_binary().expect("agent-tui binary built by cargo test");
        let mut cmd = tokio::process::Command::new(bin);
        cmd.arg("--session")
            .arg(&self.session)
            .arg("--socket-dir")
            .arg(&self.socket_dir);
        if json {
            cmd.arg("--json");
        }
        self.apply_env(&mut cmd);
        cmd
    }

    async fn output(&self, json: bool, args: &[&str]) -> Result<std::process::Output> {
        let mut cmd = self.cli(json);
        Ok(cmd.args(args).output().await?)
    }

    /// `cmd.exe /d /c <script>` with the rig env — used for the pipeline repro,
    /// where the point is `cmd`'s own handle-inheritance behavior.
    async fn cmd_exe(&self, script: &str) -> Result<std::process::Output> {
        let mut cmd = tokio::process::Command::new("cmd.exe");
        cmd.args(["/d", "/c", script]);
        self.apply_env(&mut cmd);
        Ok(cmd.output().await?)
    }

    async fn spawn_pane(&self, argv: &[&str]) -> Result<String> {
        let mut spawn_args: Vec<&str> = vec!["spawn", "--"];
        spawn_args.extend_from_slice(argv);
        let out = self.output(true, &spawn_args).await?;
        if !out.status.success() {
            bail!(
                "spawn {argv:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).with_context(|| {
            format!(
                "parse spawn response: {}",
                String::from_utf8_lossy(&out.stdout)
            )
        })?;
        find_pane_id(&v).with_context(|| format!("no pane id in spawn response: {v}"))
    }

    async fn die(&self, pane: &str) {
        let _ = self.output(false, &["die", "--pane", pane]).await;
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        // Best-effort daemon shutdown (blocking std Command — Drop is sync).
        if let Ok(bin) = agent_tui_binary() {
            let _ = std::process::Command::new(bin)
                .arg("--session")
                .arg(&self.session)
                .arg("--socket-dir")
                .arg(&self.socket_dir)
                .args(["daemon", "shutdown"])
                .env("AGENT_TUI_SOCKET_DIR", &self.socket_dir)
                .env("XDG_STATE_HOME", &self.state_home)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// First `"pane": "pN"` string anywhere in the envelope (lenient about the
/// exact nesting so the test doesn't couple to envelope layout).
fn find_pane_id(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(s)) = map.get("pane")
                && s.starts_with('p')
                && s.len() > 1
                && s[1..].chars().all(|c| c.is_ascii_digit())
            {
                return Some(s.clone());
            }
            map.values().find_map(find_pane_id)
        }
        serde_json::Value::Array(a) => a.iter().find_map(find_pane_id),
        _ => None,
    }
}

/// Poll `f` until it returns true or `max` elapses.
async fn poll_until<F, Fut>(what: &str, max: Duration, mut f: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<bool>>,
{
    let start = std::time::Instant::now();
    loop {
        if f().await? {
            return Ok(());
        }
        if start.elapsed() >= max {
            bail!("timed out ({max:?}) waiting for {what}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Locate the `dsr_probe` test binary, mirroring `agent_tui_binary`'s
/// debug-then-release candidate order.
fn dsr_probe_path() -> Result<PathBuf> {
    let root = workspace_root()?;
    for c in [
        root.join("target").join("debug").join("dsr_probe.exe"),
        root.join("target").join("release").join("dsr_probe.exe"),
    ] {
        if c.exists() {
            return Ok(c);
        }
    }
    bail!("dsr_probe.exe not built under {}/target", root.display())
}

/// Count processes whose command line contains `sentinel`, excluding the
/// querying PowerShell itself (its own command line carries the sentinel).
async fn sentinel_process_count(sentinel: &str) -> Result<usize> {
    let ps = format!(
        "(Get-CimInstance Win32_Process | Where-Object {{ $_.ProcessId -ne $PID -and $_.CommandLine -like '*{sentinel}*' }} | Measure-Object).Count"
    );
    let out = tokio::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
        .output()
        .await
        .context("spawn powershell")?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim()
        .parse::<usize>()
        .with_context(|| format!("unexpected powershell output: {s:?}"))
}

/// Pre-#124, a lazily-spawned daemon inside a `cmd.exe` pipeline inherited an
/// extra inheritable pipe handle that cmd leaves in the client, so
/// `agent-tui run ... | consumer` hung until the daemon idle timeout (15 min
/// default). The `win_spawn` handle-inheritance allow-list makes EOF prompt.
/// 90s is far under the idle timeout and far over a healthy run on a loaded
/// CI runner.
#[tokio::test]
async fn run_in_cmd_pipeline_consumer_sees_prompt_eof() -> Result<()> {
    let rig = Rig::new("run-pipe-eof")?;
    let bin = agent_tui_binary()?;
    let script = format!(
        "\"{}\" --session {} --socket-dir \"{}\" run -- cmd /c echo agent-tui-e2e-eof | findstr agent-tui-e2e-eof",
        bin.display(),
        rig.session,
        rig.socket_dir.display()
    );
    let out = tokio::time::timeout(Duration::from_secs(90), rig.cmd_exe(&script))
        .await
        .context("consumer did not see EOF within 90s (handle-inheritance regression?)")??;
    assert!(
        out.status.success(),
        "pipeline failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("agent-tui-e2e-eof"),
        "consumer saw no payload: {stdout}"
    );
    Ok(())
}

/// The engine must answer the ConPTY startup DSR (`ESC[6n`) or interactive
/// children block waiting for the cursor-position report. `dsr_probe` is the
/// minimal such child: without the `take_pty_writes` write-back its read
/// blocks forever and the sentinel never appears.
#[tokio::test]
async fn interactive_pane_answers_startup_dsr() -> Result<()> {
    let rig = Rig::new("dsr")?;
    let probe = dsr_probe_path()?;
    let probe_str = probe.to_str().context("probe path not utf-8")?;
    let pane = rig.spawn_pane(&[probe_str]).await?;
    poll_until("DSR reply on pane", Duration::from_secs(30), || async {
        let out = rig
            .output(false, &["tail", "--pane", &pane, "--strip-ansi"])
            .await?;
        Ok(String::from_utf8_lossy(&out.stdout).contains("DSR-REPLY:"))
    })
    .await?;
    let out = rig
        .output(false, &["tail", "--pane", &pane, "--strip-ansi"])
        .await?;
    let text = String::from_utf8_lossy(&out.stdout);
    // --strip-ansi removes the reply's ESC, leaving "DSR-REPLY:[<row>;<col>R".
    let reply = text.split("DSR-REPLY:").nth(1).unwrap_or("");
    assert!(
        reply.trim_end().starts_with('[') && reply.trim_end().ends_with('R'),
        "reply is not a cursor-position report: {text}"
    );
    rig.die(&pane).await;
    Ok(())
}

/// A fast child that writes-then-exits must have its trailing bytes delivered
/// before the follow stream's terminal gate (`exit_drained` / `DRAIN_GRACE`)
/// ends the stream — and the stream must end (the ConPTY EOF fix).
#[tokio::test]
async fn tail_follow_delivers_trailing_output_after_fast_exit() -> Result<()> {
    let rig = Rig::new("follow-drain")?;
    let pane = rig
        .spawn_pane(&["cmd", "/c", "for /l %i in (1,1,200) do @echo line%i"])
        .await?;
    let out = tokio::time::timeout(
        Duration::from_secs(30),
        rig.output(
            false,
            &["tail", "--pane", &pane, "--follow", "--strip-ansi"],
        ),
    )
    .await
    .context("tail --follow did not terminate (drain/EOF regression?)")??;
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("line1"), "early output missing: {text}");
    assert!(
        text.contains("line200"),
        "trailing output dropped at the terminal gate: {text}"
    );
    Ok(())
}

/// `die` must kill the whole descendant tree (`taskkill /F /T`), not just the
/// direct child. The direct child keeps itself alive with its own `ping -t`
/// so the tree root still exists at die time; the grandchild carries the
/// sentinel in its command line so `Win32_Process` can find it.
#[tokio::test]
async fn die_reaps_descendant_tree() -> Result<()> {
    let rig = Rig::new("tree-kill")?;
    let sentinel = format!(
        "e2e-tree-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let pane = rig
        .spawn_pane(&[
            "cmd",
            "/c",
            &format!(
                "start \"\" /b cmd /c \"ping -t 127.0.0.1 >nul & rem {sentinel}\" & ping -t 127.0.0.1 >nul"
            ),
        ])
        .await?;
    poll_until("grandchild to appear", Duration::from_secs(30), || async {
        Ok(sentinel_process_count(&sentinel).await? >= 1)
    })
    .await?;
    let out = rig.output(false, &["die", "--pane", &pane]).await?;
    assert!(
        out.status.success(),
        "die failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    poll_until(
        "descendant tree reaped",
        Duration::from_secs(30),
        || async { Ok(sentinel_process_count(&sentinel).await? == 0) },
    )
    .await
    .context("grandchild survived die (tree-kill regression?)")?;
    Ok(())
}

/// `signal SIGINT` writes ETX to the ConPTY input; conhost raises a real
/// `CTRL_C_EVENT` and `ping -t` exits.
#[tokio::test]
async fn sigint_via_conpty_etx_interrupts_child() -> Result<()> {
    let rig = Rig::new("sigint")?;
    let pane = rig.spawn_pane(&["ping", "-t", "127.0.0.1"]).await?;
    // Give ping a beat to be running before the interrupt lands.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let out = rig
        .output(false, &["signal", "--pane", &pane, "SIGINT"])
        .await?;
    assert!(
        out.status.success(),
        "signal SIGINT failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = rig
        .output(
            false,
            &["wait", "--pane", &pane, "--exit", "--max", "15000"],
        )
        .await?;
    assert!(
        out.status.success(),
        "child did not exit after SIGINT: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    rig.die(&pane).await;
    Ok(())
}

/// `signal SIGBREAK` has no honest delivery path to a ConPTY child, so it must
/// fail with an actionable error rather than report a false success.
#[tokio::test]
async fn sigbreak_rejected_with_actionable_error() -> Result<()> {
    let rig = Rig::new("sigbreak")?;
    let pane = rig.spawn_pane(&["ping", "-t", "127.0.0.1"]).await?;
    let out = rig
        .output(false, &["signal", "--pane", &pane, "SIGBREAK"])
        .await?;
    assert!(
        !out.status.success(),
        "SIGBREAK must not report success: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("cannot be delivered"),
        "expected the honest-error text, got: {combined}"
    );
    rig.die(&pane).await;
    Ok(())
}
