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
}

/// In-tree fixture image tags. Built locally via `just fixtures` or by
/// CI's fixture-build step before integration tests run.
pub mod fixtures {
    /// bash + `FinalTerm` OSC 133 integration baked in.
    pub const SHELL: &str = "agent-tui-fixture-shell:dev";
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
        let img = GenericImage::new(image_name, tag)
            .with_entrypoint("sleep")
            .with_cmd(["infinity"])
            .with_mount(Mount::bind_mount(
                binary.to_string_lossy().to_string(),
                AGENT_TUI_IN_CONTAINER,
            ));

        let container = img
            .start()
            .await
            .with_context(|| format!("start container {image}"))?;

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

        Ok(Self {
            container,
            artifacts: Arc::new(ArtifactDir::new(name)?),
            started_at: Instant::now(),
            name: name.to_string(),
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

    /// `agent-tui snapshot --mode outline`. Stores the result in the
    /// artifact dir so it's available for panic-time dumps.
    pub async fn snapshot(&mut self) -> Result<Snapshot> {
        let env = self
            .run_cli(
                "snapshot",
                &["snapshot".into(), "--mode".into(), "outline".into()],
            )
            .await?;
        self.artifacts.set_last_snapshot(env.clone());
        let data = env.get("data").cloned().unwrap_or(Value::Null);
        Ok(Snapshot { envelope: data })
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
        if std::thread::panicking() {
            let _ = self.artifacts.dump();
            tracing::error!(
                test = %self.name,
                artifacts = %self.artifacts.root().display(),
                "scenario panicked; artifacts captured"
            );
        }
    }
}

/// Wrapper around a snapshot response that carries the assertion helpers.
pub struct Snapshot {
    envelope: Value,
}

impl Snapshot {
    /// Whole snapshot envelope (`data` field of the agent-tui response).
    #[must_use]
    pub fn envelope(&self) -> &Value {
        &self.envelope
    }

    /// Concatenated text of every outline node's `name`.
    fn outline_text(&self) -> String {
        let nodes = self.envelope.get("outline").and_then(|o| o.get("nodes"));
        let mut out = String::new();
        if let Some(arr) = nodes.and_then(Value::as_array) {
            for n in arr {
                if let Some(name) = n.get("name").and_then(Value::as_str) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(name);
                }
            }
        }
        out
    }

    /// Assert that the rendered outline text contains `needle`. Panics
    /// (with the full outline) when it doesn't, which routes through the
    /// `Scenario::drop` artifact-capture path.
    pub fn assert_outline_contains(&self, needle: &str) -> Result<()> {
        let body = self.outline_text();
        if body.contains(needle) {
            Ok(())
        } else {
            Err(anyhow!(
                "outline does not contain {needle:?}; full outline:\n---\n{body}\n---"
            ))
        }
    }

    /// `state` field (`shell` / `running` / `alt_screen_tui` / …).
    pub fn state(&self) -> Option<&str> {
        self.envelope.get("state").and_then(Value::as_str)
    }
}

fn split_image_tag(image: &str) -> (&str, &str) {
    match image.rsplit_once(':') {
        Some((name, tag)) => (name, tag),
        None => (image, "latest"),
    }
}
