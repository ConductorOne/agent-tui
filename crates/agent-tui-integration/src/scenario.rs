//! High-level declarative DSL on top of `testcontainers` + the `agent-tui`
//! CLI. Each method maps to a single CLI subcommand the agent would use;
//! method names are imperative (`spawn`, `press`, `wait_text`) so a test
//! body reads as a script of user actions.
//!
//! Example:
//!
//! ```no_run
//! # async fn ex() -> anyhow::Result<()> {
//! use agent_tui_integration::scenario::Scenario;
//! let mut s = Scenario::new("alpine_echo_smoke", "alpine:3.20").await?;
//! s.spawn(["sh", "-c", "echo hi; sleep 60"]).await?;
//! s.wait_text("hi").await?;
//! let snap = s.snapshot().await?;
//! snap.assert_outline_contains("hi")?;
//! s.die().await?;
//! # Ok(())
//! # }
//! ```

#![allow(clippy::missing_errors_doc)]

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use testcontainers::core::{CmdWaitFor, ExecCommand, Mount};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

use crate::{ArtifactDir, CommandRecord, agent_tui_binary};

const SOCKET_DIR: &str = "/tmp/at-sock";
const STATE_DIR: &str = "/tmp/at-state";
const AGENT_TUI_IN_CONTAINER: &str = "/usr/local/bin/agent-tui";

/// One hermetic test session: a container + an agent-tui binary inside it
/// + an artifact directory the `Drop` impl flushes on panic.
pub struct Scenario {
    container: ContainerAsync<GenericImage>,
    artifacts: Arc<ArtifactDir>,
    started_at: Instant,
    name: String,
    /// Container id, cached so `Drop` (which can't `await`) can pass it
    /// to the synchronous `docker exec` / `docker logs` rescue calls.
    container_id: String,
}

/// In-tree fixture image tags. Built locally via `just fixtures` or by
/// CI's fixture-build step before integration tests run.
pub mod fixtures {
    /// bash + `FinalTerm` OSC 133 integration baked in.
    pub const SHELL: &str = "agent-tui-fixture-shell:dev";
    /// vim + a deterministic /fixtures dir + `vimtutor`.
    pub const VIM: &str = "agent-tui-fixture-vim:dev";
    /// lazygit + a seeded git repo (`/fixtures/repo`). See
    /// `fixtures/lazygit/Dockerfile` for the seeded state.
    pub const LAZYGIT: &str = "agent-tui-fixture-lazygit:dev";
}

impl Scenario {
    /// Construct and start a new scenario backed by `image` (e.g.
    /// `alpine:3.20`, `ghcr.io/ductone/agent-tui-fixtures/vim:latest`).
    ///
    /// Mounts the locally-built `agent-tui` binary at
    /// `/usr/local/bin/agent-tui` inside the container. The binary must
    /// already exist; CI runs `cargo build --bin agent-tui` before the
    /// integration test phase.
    pub async fn new(name: &str, image: &str) -> Result<Self> {
        // Initialize tracing once per process — best-effort.
        let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();

        let binary = agent_tui_binary()?;
        let (image_name, tag) = split_image_tag(image);

        // Keep the container alive with a long-running entry; we drive
        // agent-tui via `docker exec`. `sleep infinity` keeps the
        // container in `Running` until we drop the handle.
        //
        // `NetworkMode = "none"` because our scenarios are hermetic by
        // design — the daemon inside the container only talks to its
        // local Unix socket. Avoids `slirp4netns` / TUN-device
        // requirements that don't exist inside sandboxed dev pods
        // (Squire EKS, etc). We set this via `host_config_modifier`
        // because testcontainers' `with_network(...)` treats the arg as
        // a named network to attach to, not as a NetworkMode literal.
        let img = GenericImage::new(image_name, tag)
            .with_entrypoint("sleep")
            .with_cmd(["infinity"])
            .with_host_config_modifier(|host_config| {
                host_config.network_mode = Some("none".to_string());
            })
            .with_mount(Mount::bind_mount(
                binary.to_string_lossy().to_string(),
                AGENT_TUI_IN_CONTAINER,
            ));

        let container = img
            .start()
            .await
            .with_context(|| format!("start container {image}"))?;
        let container_id = container.id().to_string();

        // Ensure session dirs exist inside the container.
        for d in [SOCKET_DIR, STATE_DIR] {
            container
                .exec(
                    ExecCommand::new(["mkdir", "-p", d])
                        .with_cmd_ready_condition(CmdWaitFor::exit()),
                )
                .await
                .with_context(|| format!("mkdir {d}"))?;
        }

        let artifacts = Arc::new(ArtifactDir::new(name, image)?);
        artifacts.set_container_id(&container_id);
        Ok(Self {
            container,
            artifacts,
            started_at: Instant::now(),
            name: name.to_string(),
            container_id,
        })
    }

    /// `agent-tui spawn <argv...>`. Always uses the default session +
    /// the container-local socket/state dirs.
    pub async fn spawn<I, S>(&mut self, argv: I) -> Result<Value>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut cmd = vec!["spawn".to_string()];
        cmd.extend(argv.into_iter().map(|s| s.as_ref().to_string()));
        self.run_cli("spawn", &cmd).await
    }

    /// `agent-tui press <keys>` — drives the press-then-quiesce barrier
    /// inside the container.
    pub async fn press(&mut self, keys: &str) -> Result<Value> {
        self.run_cli("press", &["press".into(), keys.into()]).await
    }

    /// `agent-tui type <text>`.
    pub async fn type_text(&mut self, text: &str) -> Result<Value> {
        self.run_cli("type", &["type".into(), text.into()]).await
    }

    /// `agent-tui wait --text <regex>` with a generous default `--max`.
    pub async fn wait_text(&mut self, regex: &str) -> Result<Value> {
        self.run_cli(
            "wait_text",
            &[
                "wait".into(),
                "--text".into(),
                regex.into(),
                "--max".into(),
                "10000".into(),
            ],
        )
        .await
    }

    /// `agent-tui wait --idle <ms>`.
    pub async fn wait_idle(&mut self, ms: u64) -> Result<Value> {
        self.run_cli(
            "wait_idle",
            &[
                "wait".into(),
                "--idle".into(),
                ms.to_string(),
                "--max".into(),
                "10000".into(),
            ],
        )
        .await
    }

    /// `agent-tui snapshot --mode outline`. Each call is appended to the
    /// scenario's snapshot history so the artifact dump can show state
    /// evolution, not just the most recent frame.
    pub async fn snapshot(&mut self) -> Result<Snapshot> {
        let env = self
            .run_cli(
                "snapshot",
                &["snapshot".into(), "--mode".into(), "outline".into()],
            )
            .await?;
        self.artifacts.push_snapshot(env.clone());
        let data = env.get("data").cloned().unwrap_or(Value::Null);
        Ok(Snapshot::from_envelope(data))
    }

    /// `agent-tui die`.
    pub async fn die(&mut self) -> Result<Value> {
        self.run_cli("die", &["die".into()]).await
    }

    /// Eager artifact flush (e.g. after a manual `expect` failure that
    /// might not panic immediately).
    pub fn capture_now(&self) -> std::io::Result<()> {
        self.artifacts.dump()
    }

    async fn run_cli(&mut self, op: &str, args: &[String]) -> Result<Value> {
        let mut full = vec![
            AGENT_TUI_IN_CONTAINER.to_string(),
            "--socket-dir".to_string(),
            SOCKET_DIR.to_string(),
        ];
        full.extend(args.iter().cloned());

        let mut exec = self
            .container
            .exec(
                ExecCommand::new(full.clone())
                    .with_env_vars([("XDG_STATE_HOME", STATE_DIR)])
                    .with_cmd_ready_condition(CmdWaitFor::exit()),
            )
            .await
            .with_context(|| format!("exec agent-tui {op}"))?;

        let mut stdout = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut exec.stdout(), &mut stdout)
            .await
            .context("read exec stdout")?;
        let body = String::from_utf8_lossy(&stdout).trim().to_string();
        if body.is_empty() {
            return Err(anyhow!("agent-tui {op} returned no stdout"));
        }
        // The CLI returns one JSON-line response.
        let envelope: Value = serde_json::from_str(&body)
            .with_context(|| format!("parse agent-tui {op} response: {body}"))?;

        self.artifacts.record(CommandRecord {
            op: op.to_string(),
            args: serde_json::to_value(args).unwrap_or(Value::Null),
            response: envelope.clone(),
            elapsed_ms: self.started_at.elapsed().as_millis(),
        });
        Ok(envelope)
    }
}

impl Drop for Scenario {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        let _ = self.artifacts.dump();
        self.dump_diagnostics_blocking();
        tracing::error!(
            test = %self.name,
            artifacts = %self.artifacts.root().display(),
            "scenario panicked; artifacts captured"
        );
    }
}

impl Scenario {
    /// Shell out to `docker exec` / `docker logs` to fetch the cast file,
    /// PNG screenshot, and container output. Synchronous so it works from
    /// `Drop`. Failures are silent — best-effort.
    fn dump_diagnostics_blocking(&self) {
        let cid = &self.container_id;
        let docker = docker_cli();
        let artifact_root = self.artifacts.root();

        // pane.cast — the in-container recorder file for pane p1. May not
        // exist if no pane was spawned successfully.
        let cast_path = format!("{STATE_DIR}/agent-tui/default/p1.cast");
        if let Ok(out) = std::process::Command::new(&docker)
            .args(["exec", cid, "cat", &cast_path])
            .output()
            && out.status.success()
        {
            let _ = std::fs::write(artifact_root.join("pane.cast"), &out.stdout);
        }

        // last-snapshot.png — ask agent-tui inside the container to
        // rasterize one final snapshot with `--annotate` labels, then
        // copy the PNG out. Only succeeds if the daemon is still
        // healthy enough to respond, which is the common case
        // (panics happen in *test* assertion code, not the daemon).
        let png_path = format!("{STATE_DIR}/last-snapshot.png");
        let _ = std::process::Command::new(&docker)
            .args([
                "exec",
                cid,
                AGENT_TUI_IN_CONTAINER,
                "--socket-dir",
                SOCKET_DIR,
                "snapshot",
                "--mode",
                "outline",
                "--png",
                &png_path,
                "--annotate",
            ])
            .output();
        if let Ok(out) = std::process::Command::new(&docker)
            .args(["exec", cid, "cat", &png_path])
            .output()
            && out.status.success()
            && !out.stdout.is_empty()
        {
            let _ = std::fs::write(artifact_root.join("last-snapshot.png"), &out.stdout);
        }

        // container.log — what the entrypoint emitted. Usually small/empty
        // for our sleep-infinity entrypoints, but worth grabbing.
        if let Ok(out) = std::process::Command::new(&docker)
            .args(["logs", cid])
            .output()
        {
            let _ = std::fs::write(artifact_root.join("container.log"), &out.stdout);
            let _ = std::fs::write(artifact_root.join("container.err"), &out.stderr);
        }
    }
}

/// `DOCKER_CLI` env var override, else `podman` if present, else `docker`.
fn docker_cli() -> String {
    if let Ok(v) = std::env::var("DOCKER_CLI") {
        return v;
    }
    if std::process::Command::new("podman")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return "podman".into();
    }
    "docker".into()
}

// `Snapshot` lives in lib.rs so the bwrap backend can use it without
// pulling in this module's testcontainers dependency.
pub use crate::Snapshot;

fn split_image_tag(image: &str) -> (&str, &str) {
    match image.rsplit_once(':') {
        Some((name, tag)) => (name, tag),
        None => (image, "latest"),
    }
}
