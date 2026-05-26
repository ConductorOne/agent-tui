//! bwrap (bubblewrap) backend for the integration suite — a peer of the
//! Docker backend in `scenario.rs`.
//!
//! Why this exists: the Docker backend can't run in environments where
//! nested containers are blocked (Squire EKS dev pods can't `mount proc`
//! inside the container runtime). bwrap dodges that wall by sharing the
//! host's `/proc` read-only instead of remounting it, while still
//! providing user/net/IPC/UTS namespaces + a rebased rootfs.
//!
//! Architecture vs Docker:
//!
//! ```text
//!  Docker path:                       bwrap path:
//!  ┌────────────────────┐             ┌────────────────────┐
//!  │ host: testcontainers│             │ host: agent-tui   │
//!  │   ↓ docker exec    │             │   ↓ spawn         │
//!  │ container          │             │ bwrap [flags]     │
//!  │   ↓ daemon spawn   │             │   ↓ exec          │
//!  │ vim                │             │ vim               │
//!  │   (all inside the  │             │   (host daemon,   │
//!  │    container)      │             │    sandboxed vim) │
//!  └────────────────────┘             └────────────────────┘
//! ```
//!
//! The agent-tui daemon runs ON THE HOST and spawns `bwrap … -- <argv>`
//! as the PTY child. bwrap is transparent to the daemon — PTY stdio
//! flows through bwrap untouched.
//!
//! Rootfs source: `target/integration-rootfs/<fixture>/extracted/`,
//! produced by `scripts/dev/build-rootfs.sh` from the same fixture
//! Dockerfiles the Docker backend uses. Hermeticity is identical.

#![allow(clippy::missing_errors_doc)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use tokio::process::Command;

use crate::{ArtifactDir, CommandRecord, agent_tui_binary, workspace_root};

/// In-tree fixture descriptors for the bwrap backend.
///
/// `name` matches the directory under `crates/agent-tui-integration/
/// fixtures/<name>/` and the rootfs cache at `target/integration-rootfs/
/// <name>/extracted/`. The data files baked into the Dockerfile (e.g.
/// `/fixtures/sample.txt`) are present inside the extracted rootfs
/// already — no separate host-side data dir is needed.
pub mod fixtures {
    use super::BwrapFixture;

    /// vim + a deterministic `/fixtures` dir + `vimtutor`. Matches the
    /// Docker fixture at `fixtures/vim/Dockerfile`.
    pub const VIM: BwrapFixture = BwrapFixture {
        name: "vim",
        env: &[],
    };

    /// bash with `FinalTerm`/OSC 133 integration baked in at
    /// `/etc/profile.d/osc133.sh`; a login bash sources it. Tests
    /// should `bash --login -i`.
    pub const SHELL: BwrapFixture = BwrapFixture {
        name: "shell",
        env: &[],
    };

    /// lazygit + a deterministically-seeded git repo at
    /// `/fixtures/repo` (one staged file, one modified file, one
    /// untracked file, two prior commits). The fixture pre-locks every
    /// nondeterministic lazygit knob — see
    /// `fixtures/lazygit/config.yml`. Tests should
    /// `lazygit --use-config-file=/fixtures/xdg/lazygit/config.yml
    /// --path /fixtures/repo`.
    pub const LAZYGIT: BwrapFixture = BwrapFixture {
        name: "lazygit",
        env: &[
            // lazygit honors XDG_CONFIG_HOME for its config dir; the
            // Dockerfile bakes the config at /fixtures/xdg/lazygit/.
            ("XDG_CONFIG_HOME", "/fixtures/xdg"),
            ("HOME", "/fixtures"),
            ("COLORTERM", "truecolor"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
            ("GIT_CONFIG_GLOBAL", "/etc/gitconfig-fixture"),
            ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ],
    };

    /// `less` pager + a deterministic 200-line file at
    /// `/fixtures/lorem.txt`. `LESS=-M` is set so the status line
    /// carries the `lines X-Y/Z   P%` indicator scenarios anchor on.
    pub const LESS: BwrapFixture = BwrapFixture {
        name: "less",
        env: &[("LESS", "-M"), ("LANG", "C.UTF-8"), ("LC_ALL", "C.UTF-8")],
    };

    /// htop with an empty `~/.config/htop/htoprc` pre-staged so the
    /// first-run config write doesn't perturb the snapshot. Tests
    /// should launch with `-d 50 -C` for a 5-second refresh + mono
    /// output (gives `wait_idle` a long quiet window between repaints).
    pub const HTOP: BwrapFixture = BwrapFixture {
        name: "htop",
        env: &[
            ("HOME", "/root"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
        ],
    };

    /// tig + the same seeded git repo as the lazygit fixture
    /// (`/fixtures/repo`, two commits). `TIGRC_USER` and `TIGRC_SYSTEM`
    /// are pinned to a fixture-controlled rc so colors, mouse, and
    /// rev-graph are off.
    ///
    /// **`LINES` + `COLUMNS` env are explicitly set.** ncurses
    /// initializes its `stdscr` from `getmaxyx`, which reads
    /// `TIOCGWINSZ`. Under our daemon's PTY (`portable-pty` 0.9, 80x24
    /// `PtySize`), `stty -a` reports rows=24 cols=80 — but ncurses sees
    /// rows=23 at `newterm()` time and lays out the view one row short.
    /// `lazygit`, `vim`, `htop`, `less`, `fzf`, `nano` are unaffected.
    /// Forcing the env value works around the discrepancy until the
    /// root cause in the daemon PTY layer is identified.
    pub const TIG: BwrapFixture = BwrapFixture {
        name: "tig",
        env: &[
            ("NO_COLOR", "1"),
            ("TIGRC_USER", "/etc/tigrc-fixture"),
            // **Don't override TIGRC_SYSTEM** — /etc/tigrc ships ~19KB
            // of default keybindings (j/k/g/G/h/q/Enter/…) without
            // which tig has no input handlers at all and refuses to
            // paint the commit body. The user-rc we ship layers our
            // (mostly-cosmetic) overrides on top of those defaults.
            ("GIT_CONFIG_GLOBAL", "/etc/gitconfig-fixture"),
            ("GIT_CONFIG_SYSTEM", "/dev/null"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
        ],
    };

    /// fzf + a 10-item candidate list at `/fixtures/fruits.txt`.
    /// `FZF_DEFAULT_OPTS` is wiped to ignore any host config; scenarios
    /// pass explicit flags (`--no-mouse --layout=reverse`) at invoke
    /// time so the chrome layout is deterministic.
    pub const FZF: BwrapFixture = BwrapFixture {
        name: "fzf",
        env: &[
            ("FZF_DEFAULT_OPTS", ""),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
        ],
    };

    /// GNU nano + a 3-line file at `/fixtures/sample.txt`. `NO_COLOR=1`
    /// disables syntax highlighting so cell attrs stay deterministic.
    /// Scenarios should pass `-I` (no rc files) and usually `-w` (no
    /// hard wrap) at invoke time.
    pub const NANO: BwrapFixture = BwrapFixture {
        name: "nano",
        env: &[
            ("NO_COLOR", "1"),
            ("HOME", "/root"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
        ],
    };
}

/// Static descriptor for a bwrap fixture.
#[derive(Debug, Clone, Copy)]
pub struct BwrapFixture {
    /// Fixture name; matches `fixtures/<name>/` and `target/integration-rootfs/<name>/`.
    pub name: &'static str,
    /// Extra env vars injected into the sandbox via `--setenv`.
    pub env: &'static [(&'static str, &'static str)],
}

/// One bwrap-backed scenario: a unique socket+state dir on the host,
/// a daemon lazy-spawned by the first CLI call, and a per-test scratch
/// dir bound to `/work` inside the sandbox.
///
/// API mirrors `scenario::Scenario` so tests can swap backends with a
/// single import change.
pub struct BwrapScenario {
    fixture: BwrapFixture,
    rootfs: PathBuf,
    /// Per-scenario state root under `/tmp/at-bw-<8hex>/` — short
    /// because some daemon-internal Unix sockets need `sun_path` < 108.
    state_root: PathBuf,
    socket_dir: PathBuf,
    state_home: PathBuf,
    scratch: PathBuf,
    /// Host path to the agent-tui binary that runs as the daemon.
    agent_tui: PathBuf,
    artifacts: Arc<ArtifactDir>,
    started_at: Instant,
    name: String,
}

impl BwrapScenario {
    /// Construct a new bwrap-backed scenario. Verifies the fixture rootfs
    /// has been built (`just rootfs <name>`) and that `bwrap` is on PATH.
    ///
    /// `async` for API symmetry with the Docker backend's `new` — tests
    /// can swap backends with only an import change.
    #[allow(clippy::unused_async)]
    pub async fn new(name: &str, fixture: BwrapFixture) -> Result<Self> {
        let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();

        if !bwrap_on_path() {
            anyhow::bail!(
                "bwrap not found on PATH — install bubblewrap (e.g. `apt-get install bubblewrap`)"
            );
        }
        let agent_tui = agent_tui_binary()?;

        let workspace = workspace_root()?;
        let rootfs = workspace
            .join("target")
            .join("integration-rootfs")
            .join(fixture.name)
            .join("extracted");
        if !rootfs.join("usr").exists() {
            anyhow::bail!(
                "rootfs for fixture {:?} not built at {} — run `just rootfs {}`",
                fixture.name,
                rootfs.display(),
                fixture.name
            );
        }

        // Short tmp path: some daemon-internal sockets brush against
        // `sun_path`'s 108-byte limit on Linux.
        let nonce = short_id();
        let state_root = PathBuf::from(format!("/tmp/at-bw-{nonce}"));
        let socket_dir = state_root.join("s");
        let state_home = state_root.join("x");
        let scratch = state_root.join("w");
        for d in [&socket_dir, &state_home, &scratch] {
            std::fs::create_dir_all(d)
                .with_context(|| format!("create scenario dir {}", d.display()))?;
        }

        let artifacts = Arc::new(ArtifactDir::new(name, &format!("bwrap:{}", fixture.name))?);
        artifacts.set_container_id(&nonce);

        Ok(Self {
            fixture,
            rootfs,
            state_root,
            socket_dir,
            state_home,
            scratch,
            agent_tui,
            artifacts,
            started_at: Instant::now(),
            name: name.to_string(),
        })
    }

    /// `agent-tui spawn -- <bwrap flags> -- <argv>` against the daemon
    /// keyed by this scenario's socket dir.
    pub async fn spawn<I, S>(&mut self, argv: I) -> Result<Value>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let user_argv: Vec<String> = argv.into_iter().map(|s| s.as_ref().to_string()).collect();
        let mut full = vec!["spawn".to_string()];
        full.extend(self.bwrap_argv());
        full.push("--".to_string());
        full.extend(user_argv);
        self.run_cli("spawn", &full).await
    }

    /// `agent-tui press <keys>`.
    pub async fn press(&mut self, keys: &str) -> Result<Value> {
        self.run_cli("press", &["press".into(), keys.into()]).await
    }

    /// `agent-tui type <text>`.
    pub async fn type_text(&mut self, text: &str) -> Result<Value> {
        self.run_cli("type", &["type".into(), text.into()]).await
    }

    /// `agent-tui wait --text <regex> --max 10000`.
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

    /// `agent-tui wait --idle <ms> --max 10000`.
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

    /// `agent-tui snapshot --mode outline`.
    pub async fn snapshot(&mut self) -> Result<crate::Snapshot> {
        let env = self
            .run_cli(
                "snapshot",
                &["snapshot".into(), "--mode".into(), "outline".into()],
            )
            .await?;
        self.artifacts.push_snapshot(env.clone());
        let data = env.get("data").cloned().unwrap_or(Value::Null);
        Ok(crate::Snapshot::from_envelope(data))
    }

    /// `agent-tui die`.
    pub async fn die(&mut self) -> Result<Value> {
        self.run_cli("die", &["die".into()]).await
    }

    /// Eager artifact flush.
    pub fn capture_now(&self) -> std::io::Result<()> {
        self.artifacts.dump()
    }

    /// The fixed bwrap argument vector for this scenario — same flags
    /// every time, only the rootfs path and scratch dir vary.
    fn bwrap_argv(&self) -> Vec<String> {
        let mut v: Vec<String> = vec![
            "bwrap".into(),
            // Rebased rootfs.
            "--ro-bind".into(),
            self.rootfs.to_string_lossy().into_owned(),
            "/".into(),
            // Share host /proc (the trick that dodges Squire EKS's mount-proc ban).
            "--ro-bind".into(),
            "/proc".into(),
            "/proc".into(),
            // Real /dev (without it: no /dev/tty, no isatty, no PTY).
            "--dev-bind".into(),
            "/dev".into(),
            "/dev".into(),
            // Fresh tmpfs for everywhere that needs to be writable.
            "--tmpfs".into(),
            "/tmp".into(),
            "--tmpfs".into(),
            "/var/tmp".into(),
            "--tmpfs".into(),
            "/run".into(),
            "--tmpfs".into(),
            "/home".into(),
            "--tmpfs".into(),
            "/root".into(),
            // Per-test scratch bound at /work.
            "--bind".into(),
            self.scratch.to_string_lossy().into_owned(),
            "/work".into(),
            // Std env.
            "--setenv".into(),
            "HOME".into(),
            "/root".into(),
            "--setenv".into(),
            "TERM".into(),
            "xterm-256color".into(),
            "--setenv".into(),
            "PATH".into(),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into(),
            // Namespaces.
            "--unshare-user".into(),
            "--unshare-net".into(),
            "--unshare-ipc".into(),
            "--unshare-uts".into(),
            "--hostname".into(),
            "agent-tui-sandbox".into(),
            // Get killed if the daemon dies.
            "--die-with-parent".into(),
        ];
        for (k, val) in self.fixture.env {
            v.push("--setenv".into());
            v.push((*k).to_string());
            v.push((*val).to_string());
        }
        v
    }

    async fn run_cli(&mut self, op: &str, args: &[String]) -> Result<Value> {
        let mut cmd = Command::new(&self.agent_tui);
        cmd.arg("--socket-dir")
            .arg(&self.socket_dir)
            .args(args)
            .env("XDG_STATE_HOME", &self.state_home)
            .env("AGENT_TUI_SOCKET_DIR", &self.socket_dir)
            // Allow bwrap as a spawn target. The fixture's own binaries
            // (vim, bash, …) are *inside* the sandbox so no host
            // governance check applies to them; the only host process
            // is bwrap itself.
            .env("AGENT_TUI_ALLOWED_BINARIES", "*")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let out = cmd
            .output()
            .await
            .with_context(|| format!("run agent-tui {op}"))?;
        let body = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if body.is_empty() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow!(
                "agent-tui {op} returned no stdout (status={:?}, stderr={})",
                out.status.code(),
                err
            ));
        }
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

    /// Best-effort artifact rescue on panic. No `docker exec` needed —
    /// everything lives on the host filesystem already.
    fn dump_diagnostics_blocking(&self) {
        let cast_path = self
            .state_home
            .join("agent-tui")
            .join("default")
            .join("p1.cast");
        if cast_path.exists() {
            let _ = std::fs::copy(&cast_path, self.artifacts.root().join("pane.cast"));
        }

        // Best-effort PNG snapshot. Synchronous because Drop can't await.
        let png_path = self.state_root.join("last.png");
        let _ = std::process::Command::new(&self.agent_tui)
            .arg("--socket-dir")
            .arg(&self.socket_dir)
            .args([
                "snapshot",
                "--mode",
                "outline",
                "--png",
                &png_path.to_string_lossy(),
                "--annotate",
            ])
            .env("XDG_STATE_HOME", &self.state_home)
            .env("AGENT_TUI_SOCKET_DIR", &self.socket_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
        if png_path.exists() {
            let _ = std::fs::copy(&png_path, self.artifacts.root().join("last-snapshot.png"));
        }
    }

    /// Helper for tests that want to introspect the scenario's bwrap arg
    /// list (useful for golden-test debugging).
    #[must_use]
    pub fn debug_bwrap_argv(&self) -> Vec<String> {
        self.bwrap_argv()
    }

    /// Host path to the per-scenario writable scratch dir (bound to
    /// `/work` inside the sandbox). Tests that want to seed input files
    /// before `spawn(...)` write to this path.
    #[must_use]
    pub fn scratch_host_path(&self) -> &Path {
        &self.scratch
    }
}

impl Drop for BwrapScenario {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let _ = self.artifacts.dump();
            self.dump_diagnostics_blocking();
            tracing::error!(
                test = %self.name,
                artifacts = %self.artifacts.root().display(),
                "bwrap scenario panicked; artifacts captured"
            );
        }
        // Try to stop the daemon cleanly so the socket dir is reusable
        // and the host process table stays small.
        let _ = std::process::Command::new(&self.agent_tui)
            .arg("--socket-dir")
            .arg(&self.socket_dir)
            .args(["daemon", "stop"])
            .env("XDG_STATE_HOME", &self.state_home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        // Best-effort cleanup of the per-scenario state root.
        let _ = std::fs::remove_dir_all(&self.state_root);
    }
}

fn short_id() -> String {
    // 8 hex chars from a v4 uuid — plenty unique for per-test paths.
    let bytes = uuid::Uuid::new_v4().into_bytes();
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3]
    )
}

/// `bwrap --version` exits 0 iff bwrap is callable. Cheaper than a full
/// `which::which` and avoids adding a dep just for this probe.
fn bwrap_on_path() -> bool {
    std::process::Command::new("bwrap")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
