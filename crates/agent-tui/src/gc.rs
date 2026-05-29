//! `session gc` — prune dead sessions' on-disk state.
//!
//! A crash or `kill -9` leaves a session's sidecar files (`<s>.sock`,
//! `<s>.pid`, …) in the socket root and its cast files under
//! `$XDG_STATE_HOME/agent-tui/<s>/`. Nothing reaps them; they accumulate.
//! `gc` enumerates sessions across both roots, skips any whose daemon is
//! still answering its socket, and deletes the rest — optionally gated on
//! age so a session that just crashed isn't reaped out from under a
//! retry.
//!
//! The planning logic ([`plan`]) is pure over [`Candidate`]s so the
//! age/liveness policy is unit-tested without touching the filesystem or
//! spawning daemons.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use agent_tui_daemon::SocketLayout;
use agent_tui_protocol::SessionId;
use anyhow::{Context, Result};

use crate::client;

/// Sidecar file suffixes a session owns in the socket root.
const SIDECAR_SUFFIXES: &[&str] = &[
    ".sock", ".pid", ".version", ".engine", ".stream", ".metrics",
];

/// Options controlling which dead sessions get reaped.
#[derive(Debug, Clone, Copy)]
pub struct GcOptions {
    /// Only prune dead sessions whose most-recent state is at least this
    /// old. Ignored when `prune_all` is set.
    pub older_than: Duration,
    /// Prune every dead session regardless of age.
    pub prune_all: bool,
    /// Report what would be pruned without deleting anything.
    pub dry_run: bool,
}

/// One session discovered on disk, with everything `plan` needs to decide.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Session name (the basename without any sidecar suffix).
    pub session: String,
    /// Sidecar files owned by this session in the socket root.
    pub sidecars: Vec<PathBuf>,
    /// Per-session cast directory, if one exists under the state root.
    pub cast_dir: Option<PathBuf>,
    /// Age since the most-recent mtime across this session's files.
    pub age: Duration,
    /// Whether a daemon is currently answering this session's socket.
    pub alive: bool,
}

/// What a `gc` run did (or, under `--dry-run`, would do).
#[derive(Debug, Default)]
pub struct GcReport {
    /// Sessions selected for pruning.
    pub pruned: Vec<String>,
    /// Live sessions skipped.
    pub skipped_alive: usize,
    /// Dead-but-too-young sessions skipped.
    pub skipped_young: usize,
    /// Whether deletion was suppressed by `--dry-run`.
    pub dry_run: bool,
}

/// Pure policy: which candidates should be pruned. A session is reaped
/// only when it is **not** alive and either `prune_all` is set or its age
/// meets the threshold.
fn plan<'a>(candidates: &'a [Candidate], opts: &GcOptions) -> Vec<&'a Candidate> {
    candidates
        .iter()
        .filter(|c| !c.alive && (opts.prune_all || c.age >= opts.older_than))
        .collect()
}

/// Resolve roots, enumerate, probe liveness, then prune. `socket_root` and
/// `state_root` are injected so tests can drive fully isolated dirs.
///
/// # Errors
/// Surfaces filesystem errors from enumeration or deletion.
pub async fn run_gc(
    socket_root: &Path,
    state_root: Option<&Path>,
    opts: &GcOptions,
    now: SystemTime,
) -> Result<GcReport> {
    let mut candidates = gather(socket_root, state_root, now)?;
    for c in &mut candidates {
        let layout =
            SocketLayout::for_session_in(&SessionId(c.session.clone()), socket_root.to_path_buf());
        c.alive = client::probe_live(&layout).await;
    }

    let selected = plan(&candidates, opts);
    let mut report = GcReport {
        dry_run: opts.dry_run,
        skipped_alive: candidates.iter().filter(|c| c.alive).count(),
        ..Default::default()
    };
    report.skipped_young = candidates
        .iter()
        .filter(|c| !(c.alive || opts.prune_all || c.age >= opts.older_than))
        .count();

    for c in selected {
        if !opts.dry_run {
            prune(c).with_context(|| format!("prune session {}", c.session))?;
        }
        report.pruned.push(c.session.clone());
    }
    report.pruned.sort();
    Ok(report)
}

/// Enumerate sessions across the socket root and the state root, grouping
/// each session's sidecar files + cast dir and computing its age.
fn gather(
    socket_root: &Path,
    state_root: Option<&Path>,
    now: SystemTime,
) -> Result<Vec<Candidate>> {
    let mut by_session: BTreeMap<String, (Vec<PathBuf>, Option<PathBuf>, SystemTime)> =
        BTreeMap::new();

    // Socket-root sidecars: `<session><suffix>`.
    if socket_root.is_dir() {
        for entry in fs::read_dir(socket_root)
            .with_context(|| format!("read socket root {}", socket_root.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(session) = SIDECAR_SUFFIXES
                .iter()
                .find_map(|suf| name.strip_suffix(suf))
            else {
                continue;
            };
            let slot = by_session
                .entry(session.to_string())
                .or_insert_with(|| (Vec::new(), None, SystemTime::UNIX_EPOCH));
            bump_mtime(&mut slot.2, &path);
            slot.0.push(path);
        }
    }

    // State-root cast dirs: `<state_root>/agent-tui/<session>/`.
    if let Some(state_root) = state_root {
        let cast_parent = state_root.join("agent-tui");
        if cast_parent.is_dir() {
            for entry in fs::read_dir(&cast_parent)
                .with_context(|| format!("read cast root {}", cast_parent.display()))?
            {
                let entry = entry?;
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let Some(session) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let slot = by_session
                    .entry(session.to_string())
                    .or_insert_with(|| (Vec::new(), None, SystemTime::UNIX_EPOCH));
                // Most-recent mtime across the dir and its cast files —
                // an active session keeps writing casts.
                bump_mtime(&mut slot.2, &path);
                if let Ok(rd) = fs::read_dir(&path) {
                    for f in rd.flatten() {
                        bump_mtime(&mut slot.2, &f.path());
                    }
                }
                slot.1 = Some(path);
            }
        }
    }

    Ok(by_session
        .into_iter()
        .map(|(session, (sidecars, cast_dir, latest))| Candidate {
            session,
            sidecars,
            cast_dir,
            age: now.duration_since(latest).unwrap_or(Duration::ZERO),
            alive: false,
        })
        .collect())
}

/// Fold `path`'s mtime into `latest`, keeping the newer of the two.
fn bump_mtime(latest: &mut SystemTime, path: &Path) {
    if let Ok(m) = fs::metadata(path).and_then(|md| md.modified()) {
        if m > *latest {
            *latest = m;
        }
    }
}

/// Delete a candidate's sidecar files and cast directory.
fn prune(c: &Candidate) -> Result<()> {
    for f in &c.sidecars {
        match fs::remove_file(f) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("remove {}", f.display())),
        }
    }
    if let Some(dir) = &c.cast_dir {
        match fs::remove_dir_all(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("remove {}", dir.display())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(session: &str, alive: bool, age_secs: u64) -> Candidate {
        Candidate {
            session: session.to_string(),
            sidecars: Vec::new(),
            cast_dir: None,
            age: Duration::from_secs(age_secs),
            alive,
        }
    }

    fn opts(older_than_secs: u64, prune_all: bool) -> GcOptions {
        GcOptions {
            older_than: Duration::from_secs(older_than_secs),
            prune_all,
            dry_run: false,
        }
    }

    #[test]
    fn plan_never_prunes_a_live_session() {
        let cands = vec![cand("live", true, 999_999)];
        assert!(plan(&cands, &opts(1, true)).is_empty());
    }

    #[test]
    fn plan_respects_age_threshold() {
        let cands = vec![cand("young", false, 100), cand("old", false, 10_000)];
        let got: Vec<_> = plan(&cands, &opts(1_000, false))
            .iter()
            .map(|c| c.session.as_str())
            .collect();
        assert_eq!(got, ["old"]);
    }

    #[test]
    fn plan_all_ignores_age_for_dead_sessions() {
        let cands = vec![cand("young", false, 1), cand("live", true, 99_999)];
        let got: Vec<_> = plan(&cands, &opts(1_000, true))
            .iter()
            .map(|c| c.session.as_str())
            .collect();
        assert_eq!(
            got,
            ["young"],
            "prune_all reaps dead regardless of age, never live"
        );
    }

    /// Unique throwaway dir under the system tmpdir.
    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "agent-tui-gc-{}-{}-{tag}",
            std::process::id(),
            // Cheap intra-process uniqueness.
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn touch(path: &Path) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, b"x").unwrap();
    }

    #[test]
    fn gather_groups_sidecars_and_cast_dir_per_session() {
        let sock = temp_dir("sock");
        let state = temp_dir("state");
        touch(&sock.join("alice.sock"));
        touch(&sock.join("alice.pid"));
        touch(&sock.join("bob.version"));
        touch(&state.join("agent-tui").join("alice").join("p1.cast"));
        touch(&state.join("agent-tui").join("carol").join("p1.cast"));
        // A non-session file in the socket root must be ignored.
        touch(&sock.join("notes.txt"));

        let mut got = gather(&sock, Some(&state), SystemTime::now()).unwrap();
        got.sort_by(|a, b| a.session.cmp(&b.session));
        let names: Vec<_> = got.iter().map(|c| c.session.as_str()).collect();
        assert_eq!(names, ["alice", "bob", "carol"]);

        let alice = &got[0];
        assert_eq!(alice.sidecars.len(), 2);
        assert!(alice.cast_dir.is_some());
        let carol = &got[2];
        assert!(carol.sidecars.is_empty());
        assert!(carol.cast_dir.is_some(), "cast-only session is discovered");

        fs::remove_dir_all(&sock).ok();
        fs::remove_dir_all(&state).ok();
    }

    #[tokio::test]
    async fn run_gc_reaps_dead_and_respects_dry_run_and_age() {
        let sock = temp_dir("e2e-sock");
        let state = temp_dir("e2e-state");
        // `gone` is a stale dead session (regular files; nothing listening
        // on `gone.sock`, so probe_live → false).
        touch(&sock.join("gone.sock"));
        touch(&sock.join("gone.pid"));
        let cast = state.join("agent-tui").join("gone");
        touch(&cast.join("p1.cast"));

        let now = SystemTime::now();
        let opts = GcOptions {
            older_than: Duration::ZERO,
            prune_all: true,
            dry_run: true,
        };

        // Dry run: reports the session but deletes nothing.
        let report = run_gc(&sock, Some(&state), &opts, now).await.unwrap();
        assert_eq!(report.pruned, ["gone"]);
        assert!(report.dry_run);
        assert!(sock.join("gone.sock").exists(), "dry-run must not delete");
        assert!(cast.exists(), "dry-run must not delete cast dir");

        // Real run: files gone.
        let opts = GcOptions {
            dry_run: false,
            ..opts
        };
        let report = run_gc(&sock, Some(&state), &opts, now).await.unwrap();
        assert_eq!(report.pruned, ["gone"]);
        assert!(!sock.join("gone.sock").exists());
        assert!(!sock.join("gone.pid").exists());
        assert!(!cast.exists());

        fs::remove_dir_all(&sock).ok();
        fs::remove_dir_all(&state).ok();
    }
}
