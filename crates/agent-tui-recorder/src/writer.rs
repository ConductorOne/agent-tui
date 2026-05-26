//! NDJSON writer with size-based rotation.
//!
//! One `Recorder` owns a directory plus the current file. `push_*` methods
//! enqueue events into a bounded channel that a dedicated tokio task drains
//! to disk. Rotation kicks in once the current file passes the configured
//! cap; the rotated file is gzipped to `<basename>.NNNN.cast.gz`.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result};
use flate2::Compression;
use flate2::write::GzEncoder;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::Event;

/// Default rotation threshold: 16 MiB matches RFC §10.2.
pub const DEFAULT_ROTATE_BYTES: u64 = 16 * 1024 * 1024;
/// Bounded channel capacity (events, not bytes).
pub const CHANNEL_CAPACITY: usize = 4096;
/// Default per-session retention cap: 1 GiB total `.gz` files (matches §10.2).
pub const DEFAULT_RETENTION_BYTES: u64 = 1024 * 1024 * 1024;

/// Recorder configuration captured at construction time.
#[derive(Debug, Clone)]
pub struct RecorderConfig {
    /// Directory where `.cast` and rotated `.cast.gz` files live.
    pub dir: PathBuf,
    /// Basename for the cast file (typically the pane id, e.g. `p1`).
    pub basename: String,
    /// Bytes-per-file rotation threshold.
    pub rotate_bytes: u64,
    /// Total `.gz` retention cap. Evicted oldest-first on each rotation.
    pub retention_bytes: u64,
}

impl RecorderConfig {
    /// Construct a default-rotation config.
    #[must_use]
    pub fn new(dir: PathBuf, basename: impl Into<String>) -> Self {
        Self {
            dir,
            basename: basename.into(),
            rotate_bytes: DEFAULT_ROTATE_BYTES,
            retention_bytes: DEFAULT_RETENTION_BYTES,
        }
    }

    /// Path to the active cast file.
    #[must_use]
    pub fn current_path(&self) -> PathBuf {
        self.dir.join(format!("{}.cast", self.basename))
    }
}

/// Statistics surfaced for monitoring + tests.
#[derive(Debug, Default, Clone)]
pub struct RecorderStats {
    /// Number of events successfully written.
    pub events_written: u64,
    /// Number of events dropped because the channel was full.
    pub events_dropped: u64,
    /// Number of rotations performed in this Recorder's lifetime.
    pub rotations: u64,
    /// Number of `.cast.gz` files evicted by retention.
    pub evictions: u64,
}

/// Handle the daemon holds; clones share a single writer task.
#[derive(Clone)]
pub struct Recorder {
    tx: mpsc::Sender<Event>,
    stats: Arc<Mutex<RecorderStats>>,
    started_at: SystemTime,
}

impl Recorder {
    /// Spawn the writer task and return a handle. Creates `cfg.dir` if missing.
    pub fn start(cfg: RecorderConfig) -> Result<(Self, JoinHandle<()>)> {
        std::fs::create_dir_all(&cfg.dir).context("create recorder dir")?;
        let (tx, rx) = mpsc::channel::<Event>(CHANNEL_CAPACITY);
        let stats = Arc::new(Mutex::new(RecorderStats::default()));
        let task_stats = stats.clone();
        let handle = tokio::task::spawn_blocking(move || {
            writer_loop(&cfg, rx, &task_stats);
        });
        Ok((
            Self {
                tx,
                stats,
                started_at: SystemTime::now(),
            },
            handle,
        ))
    }

    /// Push an `o` (output) event.
    pub fn push_output(&self, bytes: &[u8]) {
        let t = self.elapsed();
        self.try_send(crate::output_event(t, bytes));
    }

    /// Push an `i` (input) event. Pass `None` to write a null placeholder
    /// (used by `auth use`).
    pub fn push_input(&self, bytes: Option<&[u8]>) {
        let t = self.elapsed();
        self.try_send(crate::input_event(t, bytes));
    }

    /// Push an `r` (resize) event.
    pub fn push_resize(&self, cols: u16, rows: u16) {
        let t = self.elapsed();
        self.try_send(crate::resize_event(t, cols, rows));
    }

    /// Push an `s` (sequence checkpoint) event.
    pub fn push_checkpoint(&self, seq: u64, hash: &str) {
        let t = self.elapsed();
        self.try_send(crate::checkpoint_event(t, seq, hash));
    }

    /// Push an `m` (tool-call marker) event.
    pub fn push_marker(&self, kind: &str, command: &str, ok: bool, err: Option<&str>) {
        let t = self.elapsed();
        self.try_send(crate::marker_event(t, kind, command, ok, err));
    }

    /// Snapshot current stats.
    pub async fn stats(&self) -> RecorderStats {
        self.stats.lock().await.clone()
    }

    fn elapsed(&self) -> f64 {
        self.started_at.elapsed().map_or(0.0, |d| d.as_secs_f64())
    }

    fn try_send(&self, evt: Event) {
        // try_send is non-blocking — drops on full so we never stall the
        // hot path of a reader task.
        if self.tx.try_send(evt).is_err() {
            // Stats update is best-effort; if the lock is contended we drop
            // the count update along with the event.
            if let Ok(mut s) = self.stats.try_lock() {
                s.events_dropped = s.events_dropped.saturating_add(1);
            }
        }
    }
}

fn writer_loop(
    cfg: &RecorderConfig,
    mut rx: mpsc::Receiver<Event>,
    stats: &Arc<Mutex<RecorderStats>>,
) {
    let mut file = match open_current(cfg) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, "recorder failed to open initial file");
            return;
        }
    };
    let mut written_bytes: u64 = file_size(&cfg.current_path()).unwrap_or(0);

    while let Some(evt) = rx.blocking_recv() {
        if let Err(e) = write_one(&mut file, &evt) {
            tracing::warn!(error = %e, "recorder write failed; dropping event");
            continue;
        }
        let line_len = serialized_len(&evt);
        written_bytes = written_bytes.saturating_add(line_len);
        if let Ok(mut s) = stats.try_lock() {
            s.events_written = s.events_written.saturating_add(1);
        }
        if written_bytes >= cfg.rotate_bytes {
            match rotate(cfg, &mut file) {
                Ok(()) => {
                    written_bytes = 0;
                    if let Ok(mut s) = stats.try_lock() {
                        s.rotations = s.rotations.saturating_add(1);
                    }
                    let evicted = enforce_retention(cfg);
                    if evicted > 0
                        && let Ok(mut s) = stats.try_lock()
                    {
                        s.evictions = s.evictions.saturating_add(evicted);
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "recorder rotation failed; continuing on current file");
                }
            }
        }
    }
}

/// Sum sizes of `.cast.gz` files in the recorder dir; evict oldest-first
/// until under `retention_bytes`. Returns the number of files evicted.
fn enforce_retention(cfg: &RecorderConfig) -> u64 {
    let prefix = format!("{}.", cfg.basename);
    let suffix = ".cast.gz";
    let mut entries: Vec<(PathBuf, u64, SystemTime)> = match std::fs::read_dir(&cfg.dir) {
        Ok(d) => d
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if !name.starts_with(&prefix) || !name.ends_with(suffix) {
                    return None;
                }
                let md = e.metadata().ok()?;
                Some((e.path(), md.len(), md.modified().ok()?))
            })
            .collect(),
        Err(_) => return 0,
    };
    let total: u64 = entries.iter().map(|(_, s, _)| s).sum();
    if total <= cfg.retention_bytes {
        return 0;
    }
    entries.sort_by_key(|(_, _, mtime)| *mtime);
    let mut evicted = 0u64;
    let mut remaining = total;
    for (path, size, _) in entries {
        if remaining <= cfg.retention_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            remaining = remaining.saturating_sub(size);
            evicted += 1;
        }
    }
    evicted
}

fn open_current(cfg: &RecorderConfig) -> Result<BufWriter<File>> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(cfg.current_path())
        .context("open cast file")?;
    Ok(BufWriter::new(f))
}

fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

fn write_one(out: &mut BufWriter<File>, evt: &Event) -> Result<()> {
    serde_json::to_writer(&mut *out, &(evt.time, evt.kind, &evt.payload))?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

fn serialized_len(evt: &Event) -> u64 {
    serde_json::to_vec(&(evt.time, evt.kind, &evt.payload)).map_or(0, |b| (b.len() + 1) as u64)
}

fn rotate(cfg: &RecorderConfig, current: &mut BufWriter<File>) -> Result<()> {
    current.flush().context("flush before rotate")?;
    let dest = next_rotated_path(cfg);
    // Read the current file as a single chunk and gzip-encode into the rotated path.
    let src = cfg.current_path();
    let raw = std::fs::read(&src).context("read current cast")?;
    let gz_file = File::create(&dest).context("create rotated gz")?;
    let mut gz = GzEncoder::new(BufWriter::new(gz_file), Compression::default());
    gz.write_all(&raw).context("gz write")?;
    gz.finish().context("gz finish")?;
    // Truncate the current file for re-use.
    *current = BufWriter::new(
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&src)
            .context("reopen current after rotate")?,
    );
    Ok(())
}

fn next_rotated_path(cfg: &RecorderConfig) -> PathBuf {
    // Numeric suffix that increments past existing files. Naming:
    // `<basename>.0001.cast.gz`, `<basename>.0002.cast.gz`, ...
    let mut n = 1u32;
    loop {
        let candidate = cfg.dir.join(format!("{}.{:04}.cast.gz", cfg.basename, n));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
        if n > 9_999 {
            // Safety: collapse onto the highest slot; admin will clean.
            return cfg.dir.join(format!("{}.9999.cast.gz", cfg.basename));
        }
    }
}
