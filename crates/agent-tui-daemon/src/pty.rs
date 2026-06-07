//! PTY child process wrapper.
//!
//! Owns one `portable-pty` master + slave pair and the spawned child. Bytes
//! from the slave end stream into an `Engine::feed` call via a blocking
//! reader task; input bytes from `write_input` go back the other way.
//!
//! The reader task lives for the life of the child — it exits when the slave
//! closes its end (child exit, EOF) or when an I/O error is returned.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use agent_tui_engine::Engine;
use agent_tui_protocol::request::StdinMode;
use agent_tui_recorder::Recorder;
use anyhow::{Context, Result, anyhow};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// A spawned PTY child, paired with the engine that consumes its output.
///
/// Every internal handle that `portable-pty` returns is `Send` but **not**
/// `Sync` (no shared-reference invariants are guaranteed by the underlying
/// fd-owning types), so we wrap each in a `Mutex` to make the whole struct
/// `Send + Sync` for storage inside `Arc<Pane>`.
pub struct PtyChild {
    master: MasterEnd,
    writer: Mutex<Box<dyn Write + Send>>,
    /// Separate write end of the child's stdin when `stdin_mode == Pipe`.
    /// `None` for `Pty` (writes go through `writer`) and `Closed` (stdin
    /// is /dev/null, no writer at all).
    stdin_pipe: Mutex<Option<std::fs::File>>,
    /// Captured at spawn so callers can branch (e.g. `close-stdin` is a
    /// no-op for `Pty`/`Closed` modes — they have no separate pipe).
    stdin_mode: StdinMode,
    /// Rolling buffer of bytes the child has written to stdout+stderr.
    /// Capped at [`OUTPUT_BUFFER_CAP`]; exposes the raw byte stream to
    /// `tail` (RFC: subprocess-as-data pattern). Tracks total bytes
    /// ever observed in `output_total` so callers using byte offsets
    /// know whether they missed any data past the buffer's tail edge.
    output_buf: Arc<Mutex<OutputRing>>,
    /// Serializes each reader chunk's `engine.feed` + `ring.push` against an
    /// `attach` capture, so a rendered frame and the ring high-water mark can
    /// be read with **no byte falling between them** (the prelude→follow seam
    /// atomicity contract). Held briefly per chunk by the reader and once per
    /// `capture()` by an attacher.
    capture_lock: Arc<Mutex<()>>,
    child: ChildEnd,
    /// Reader task handle; kept so callers can abort/await on drop.
    reader: Mutex<Option<JoinHandle<()>>>,
    /// Optional recorder so input events can be teed back into the cast log.
    recorder: Option<Recorder>,
    /// First ~512 bytes seen from the PTY. Used for late adapter re-detection.
    first_bytes: Arc<Mutex<FirstBytes>>,
    /// Most recent OSC 133 marker seen on this PTY, used by the shell-state
    /// classifier. Updated by the reader task; read by the snapshot handler.
    last_osc133: Arc<Mutex<Option<crate::osc133::Marker>>>,
    /// Set once the reader task observes EOF (or an error) on the PTY
    /// master and exits — i.e. every byte the child ever wrote is now in
    /// the output ring. The streaming `tail --follow` path waits for this
    /// before emitting its terminal `eof` so a fast-exiting child's final
    /// output isn't dropped (the child can be reaped by `try_wait` a beat
    /// before the reader thread flushes the last bytes).
    reader_done: Arc<AtomicBool>,
    /// Notify-on-append channel: the reader task publishes the ring's new
    /// high-water mark here after every chunk push. Streaming followers
    /// subscribe via [`Self::subscribe_output`] and block on `changed()`
    /// instead of polling, collapsing echo latency to the ring-freshness
    /// floor. The held sender keeps the channel alive for late subscribers.
    output_tx: watch::Sender<u64>,
}

/// The master end of the PTY, abstracted over the two ways a [`PtyChild`]
/// comes to own one:
///
///  - [`MasterEnd::Portable`] — the normal `spawn` path, where `portable-pty`
///    allocated the pair and hands us a `MasterPty` trait object.
///  - [`MasterEnd::Adopted`] — the in-place-upgrade path, where the new daemon
///    image inherited a raw master fd (by number, across `execve`) from the
///    previous image. There is no `portable-pty` object to rebuild — exec
///    nuked it — so we hold the fd directly and drive resize/pgid via raw
///    `ioctl`/`tcgetpgrp` on it.
enum MasterEnd {
    Portable(Mutex<Box<dyn MasterPty + Send>>),
    #[cfg(unix)]
    Adopted(Mutex<std::os::fd::OwnedFd>),
}

/// The child process, abstracted over `spawn` vs `adopt`.
///
///  - [`ChildEnd::Portable`] — `portable-pty`'s `Child` + `ChildKiller`.
///  - [`ChildEnd::Adopted`] — after a same-PID re-exec the daemon is STILL the
///    child's parent, so `waitpid` works directly. We track the pid plus a
///    memoized exit code (set the first time `waitpid` reports termination).
enum ChildEnd {
    Portable {
        child: Mutex<Box<dyn Child + Send + Sync>>,
        killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    },
    #[cfg(unix)]
    Adopted {
        pid: i32,
        /// Memoized shell-style exit code (signal death → 128+sig), once the
        /// adopted child has been reaped via `waitpid`.
        exited: Mutex<Option<u32>>,
    },
}

/// Holds the first ~`MAX_FIRST_BYTES` bytes of PTY output for the adapter
/// re-detection pass that runs shortly after spawn.
#[derive(Default)]
struct FirstBytes {
    buf: Vec<u8>,
    done: bool,
}

const MAX_FIRST_BYTES: usize = 512;

/// Maximum bytes retained by the rolling output buffer used by `tail`.
/// Sized so that an agent reading once per second won't outpace it for
/// typical output rates; older bytes are evicted. Total-bytes-ever
/// counter keeps the cursor coherent even when eviction happened.
const OUTPUT_BUFFER_CAP: usize = 1_048_576;

/// Bounded byte ring used by [`PtyChild::tail`]. Stores up to
/// [`OUTPUT_BUFFER_CAP`] bytes; the eviction policy is FIFO. Callers
/// pass a `since` byte offset (cumulative, monotonic) and get back
/// whatever's still in the buffer at or after that offset, plus the
/// current high-water mark.
#[derive(Default)]
struct OutputRing {
    /// Cumulative bytes ever observed. Monotonic.
    total: u64,
    /// Most recent up-to-CAP bytes. Front is oldest.
    bytes: std::collections::VecDeque<u8>,
}

impl OutputRing {
    fn push(&mut self, chunk: &[u8]) {
        self.total = self.total.saturating_add(chunk.len() as u64);
        self.bytes.extend(chunk);
        while self.bytes.len() > OUTPUT_BUFFER_CAP {
            self.bytes.pop_front();
        }
    }

    /// Bytes since `offset` cumulative-byte-mark, plus whether the
    /// requested offset was before the buffer's tail (data lost).
    fn since(&self, offset: u64) -> TailRead {
        let tail_offset = self.total.saturating_sub(self.bytes.len() as u64);
        if offset >= self.total {
            return TailRead {
                bytes: Vec::new(),
                lost_bytes: 0,
                total: self.total,
            };
        }
        let (data_start, lost_bytes) = if offset < tail_offset {
            (0usize, tail_offset - offset)
        } else {
            // Safe: `offset >= tail_offset` and the buffer has up to
            // CAP bytes (~ usize on real platforms).
            #[allow(clippy::cast_possible_truncation)]
            ((offset - tail_offset) as usize, 0u64)
        };
        let bytes: Vec<u8> = self.bytes.iter().copied().skip(data_start).collect();
        TailRead {
            bytes,
            lost_bytes,
            total: self.total,
        }
    }
}

/// Result of a `tail` read.
pub struct TailRead {
    /// The bytes from the buffer at or after the requested offset.
    pub bytes: Vec<u8>,
    /// Number of bytes that were already evicted from the ring before
    /// `since`. Non-zero means the caller passed a stale cursor.
    pub lost_bytes: u64,
    /// Current high-water mark (cumulative bytes ever observed).
    pub total: u64,
}

impl PtyChild {
    /// Spawn `argv` under a fresh PTY of size `(cols, rows)` and start the
    /// reader task piping output into `engine.feed`. `recorder`, when
    /// supplied, gets a tee of every byte chunk read from the PTY.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn spawn(
        argv: &[String],
        cwd: Option<&Path>,
        cols: u16,
        rows: u16,
        engine: Arc<dyn Engine>,
        recorder: Option<Recorder>,
        stdin_mode: StdinMode,
        env: &[(String, String)],
    ) -> Result<Self> {
        if argv.is_empty() {
            return Err(anyhow!("argv must be non-empty"));
        }
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty")?;

        // **Normalize LINES + COLUMNS to match the PTY we just allocated.**
        //
        // ncurses-based programs (tig, mc, dialog, …) read `LINES` and
        // `COLUMNS` from the environment first and only fall back to
        // `TIOCGWINSZ` when those env vars are unset. portable-pty's
        // CommandBuilder inherits the parent process's env, so the
        // daemon's own LINES/COLUMNS — which come from the user's
        // *outer* shell, not our PTY — would leak through and convince
        // ncurses the screen is the outer-shell size (often 50+ rows,
        // 200+ cols on modern monitors) while the actual PTY is 80×24.
        // tig with that mismatch draws into virtual rows past the actual
        // grid; the visible result is a mostly-blank pane with chrome
        // displaced to the bottom rows. Other engines (vim, less) that
        // consult TIOCGWINSZ instead of LINES weren't affected, which
        // made the bug look TUI-specific. Forcing the env to match the
        // PTY removes the discrepancy.
        // Final env list: defaults first (so a user `env` of `LINES=foo`
        // or `TERM=screen` can deliberately override them), then the
        // caller's per-spawn overrides. portable-pty's CommandBuilder
        // applies each `cmd.env(k, v)` in iteration order, so later
        // entries win.
        //
        // `TERM` matters: vim/tmux/less consult terminfo to decide
        // whether to switch to the alt-screen (ESC[?1049h). With no
        // TERM, vim falls back to a primary-screen render path and the
        // pane never reports `alt_screen=true`. portable-pty inherits
        // env from the daemon process, so this only bites when the
        // daemon itself was launched in a stripped-env context (e.g.
        // inside a freshly-exec'd Docker container).
        let mut env_overrides: Vec<(String, String)> = vec![
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("LINES".to_string(), rows.to_string()),
            ("COLUMNS".to_string(), cols.to_string()),
        ];
        env_overrides.extend(env.iter().cloned());

        let (child, stdin_pipe): (Box<dyn Child + Send + Sync>, Option<std::fs::File>) =
            match stdin_mode {
                StdinMode::Pty => {
                    // portable-pty spawn, slave PTY on all three FDs. We keep
                    // its robust slave-fd handling (no re-open by `ptsname`,
                    // which is fragile under remounted `/dev/pts` in sandboxes);
                    // signal-death exit codes are recovered faithfully in
                    // `shell_exit_code` from the child's reported `.signal()`.
                    let mut cmd = CommandBuilder::new(&argv[0]);
                    for arg in &argv[1..] {
                        cmd.arg(arg);
                    }
                    if let Some(d) = cwd {
                        cmd.cwd(d);
                    }
                    for (k, v) in &env_overrides {
                        cmd.env(k, v);
                    }
                    let c = pair.slave.spawn_command(cmd).context("spawn child")?;
                    (c, None)
                }
                StdinMode::Pipe | StdinMode::Closed => {
                    // Custom path: we need stdin to be a pipe (or /dev/null)
                    // while stdout/stderr stay on the slave PTY. portable-pty's
                    // `spawn_command` hardcodes all three FDs to the slave, so
                    // we replicate just the bits we need on top of the
                    // already-allocated PTY pair. Pass the master because the
                    // slave fd isn't exposed by portable-pty's trait — we
                    // re-derive it via ptsname().
                    //
                    // Windows: the custom-stdin path is Unix-only because it
                    // uses ptsname + dup + setsid. On Windows the spawn
                    // returns an error pointing at the limitation; the
                    // Windows port will land separately (see
                    // `docs/windows-strategy.md`).
                    #[cfg(unix)]
                    {
                        spawn_with_custom_stdin(
                            argv,
                            cwd,
                            &env_overrides,
                            &*pair.master,
                            stdin_mode,
                        )?
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = (argv, cwd, &env_overrides, &pair, stdin_mode);
                        return Err(anyhow!(
                            "stdin mode {:?} requires Unix; Windows support is tracked in docs/windows-strategy.md",
                            stdin_mode
                        ));
                    }
                }
            };
        let killer = child.clone_killer();

        // Drop the slave end so the kernel closes it on child exit and the
        // master read loop sees EOF instead of hanging.
        drop(pair.slave);

        let writer = pair.master.take_writer().context("take_writer")?;
        let reader = pair.master.try_clone_reader().context("clone_reader")?;

        let reader_engine = engine;
        let reader_recorder = recorder.clone();
        let first_bytes = Arc::new(Mutex::new(FirstBytes::default()));
        let reader_first_bytes = first_bytes.clone();
        let last_osc133 = Arc::new(Mutex::new(None));
        let reader_osc133 = last_osc133.clone();
        let output_buf = Arc::new(Mutex::new(OutputRing::default()));
        let reader_output_buf = output_buf.clone();
        let capture_lock = Arc::new(Mutex::new(()));
        let reader_capture_lock = capture_lock.clone();
        let reader_done = Arc::new(AtomicBool::new(false));
        let reader_done_signal = reader_done.clone();
        // Notify-on-append channel. The reader task publishes the ring's new
        // high-water mark; followers block on `changed()` for low-latency
        // streaming. The retained sender (`output_tx` below) keeps it open.
        let (output_tx, _output_rx) = watch::channel(0u64);
        let reader_output_tx = output_tx.clone();
        let reader_handle = tokio::task::spawn_blocking(move || {
            pty_reader_loop(
                reader,
                &reader_engine,
                reader_recorder.as_ref(),
                &reader_first_bytes,
                &reader_osc133,
                &reader_output_buf,
                &reader_capture_lock,
                &reader_output_tx,
            );
            // Reader saw EOF/error and is exiting: every byte the child
            // wrote is now in the output ring. Publish *after* the loop so
            // a `reader_finished()` observer is guaranteed to see a fully
            // drained ring.
            reader_done_signal.store(true, Ordering::Release);
        });

        Ok(Self {
            master: MasterEnd::Portable(Mutex::new(pair.master)),
            writer: Mutex::new(writer),
            stdin_pipe: Mutex::new(stdin_pipe),
            stdin_mode,
            output_buf,
            capture_lock,
            child: ChildEnd::Portable {
                child: Mutex::new(child),
                killer: Mutex::new(killer),
            },
            reader: Mutex::new(Some(reader_handle)),
            recorder,
            first_bytes,
            last_osc133,
            reader_done,
        })
    }

    /// Adopt a PTY master fd + child process inherited across an in-place
    /// daemon re-exec (`daemon upgrade`, Option-A).
    ///
    /// `master_fd` is a raw fd the previous daemon image left open across
    /// `execve` (it cleared `FD_CLOEXEC` first). We take ownership of it,
    /// derive blocking reader + writer handles from `dup`s, and start the
    /// same reader loop `spawn` uses — feeding `engine` and (re-)recording
    /// into `recorder`. `child_pid` is the surviving child; because the
    /// re-exec preserved the daemon's PID, `waitpid(child_pid)` still works,
    /// so exit-code fidelity carries over for free. `last_exit` seeds the
    /// memoized exit code for a pane that had already terminated before the
    /// upgrade (terminal-retained).
    #[cfg(unix)]
    pub fn adopt(
        master_fd: std::os::fd::RawFd,
        child_pid: i32,
        last_exit: Option<i32>,
        engine: Arc<dyn Engine>,
        recorder: Option<Recorder>,
    ) -> Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        // Take ownership of the inherited fd.
        #[allow(unsafe_code)]
        let owned = unsafe { OwnedFd::from_raw_fd(master_fd) };

        // PTY masters are normally blocking; clear O_NONBLOCK defensively so
        // the blocking reader loop doesn't spin on EAGAIN if the prior image
        // had flipped it.
        clear_nonblocking(owned.as_raw_fd());

        // Independent dup'd handles for the reader and writer so each owns its
        // own fd (closing one doesn't disturb the canonical master or the
        // other), mirroring how `portable-pty` hands out a reader + writer.
        let reader_fd = dup_owned(&owned)?;
        let writer_fd = dup_owned(&owned)?;
        let reader: Box<dyn Read + Send> = Box::new(std::fs::File::from(reader_fd));
        let writer: Box<dyn Write + Send> = Box::new(std::fs::File::from(writer_fd));

        let reader_engine = engine;
        let reader_recorder = recorder.clone();
        let first_bytes = Arc::new(Mutex::new(FirstBytes::default()));
        let reader_first_bytes = first_bytes.clone();
        let last_osc133 = Arc::new(Mutex::new(None));
        let reader_osc133 = last_osc133.clone();
        let output_buf = Arc::new(Mutex::new(OutputRing::default()));
        let reader_output_buf = output_buf.clone();
        let capture_lock = Arc::new(Mutex::new(()));
        let reader_capture_lock = capture_lock.clone();
        let reader_done = Arc::new(AtomicBool::new(false));
        let reader_done_signal = reader_done.clone();
        let reader_handle = tokio::task::spawn_blocking(move || {
            pty_reader_loop(
                reader,
                &reader_engine,
                reader_recorder.as_ref(),
                &reader_first_bytes,
                &reader_osc133,
                &reader_output_buf,
                &reader_capture_lock,
            );
            reader_done_signal.store(true, Ordering::Release);
        });

        Ok(Self {
            master: MasterEnd::Adopted(Mutex::new(owned)),
            writer: Mutex::new(writer),
            // The original stdin pipe (if any) did not survive the exec; an
            // adopted pane writes input through the PTY master like `Pty` mode.
            stdin_pipe: Mutex::new(None),
            stdin_mode: StdinMode::Pty,
            output_buf,
            capture_lock,
            child: ChildEnd::Adopted {
                pid: child_pid,
                #[allow(clippy::cast_sign_loss)]
                exited: Mutex::new(last_exit.map(|c| c as u32)),
            },
            reader: Mutex::new(Some(reader_handle)),
            recorder,
            first_bytes,
            last_osc133,
            reader_done,
            output_tx,
        })
    }

    /// Read raw output bytes the child has written since `since` (a
    /// cumulative byte offset). Returns the current high-water mark
    /// plus the bytes from the ring buffer. If `since` is before what
    /// the ring still holds, `lost_bytes` is set.
    pub fn tail(&self, since: u64) -> TailRead {
        let g = self.output_buf.lock().expect("output_buf poisoned");
        g.since(since)
    }

    /// Atomically capture some derived value `f()` (e.g. an engine snapshot)
    /// **together with** the ring's current high-water mark, holding the
    /// capture lock so the reader can't feed+push a chunk in between. Returns
    /// `(f_result, high_water)`: `high_water` is the offset of the first byte
    /// NOT yet reflected in `f_result`, so an attacher can follow from exactly
    /// there with no gap and no double-paint.
    pub fn capture<R>(&self, f: impl FnOnce() -> R) -> (R, u64) {
        let _cap = self.capture_lock.lock().expect("capture_lock poisoned");
        let value = f();
        let total = self.output_buf.lock().expect("output_buf poisoned").total;
        (value, total)
    }

    /// Write bytes to the child's stdin **pipe** (when spawned with
    /// `StdinMode::Pipe`). Returns an error for other modes, where
    /// stdin bytes go through the PTY via [`Self::write_input`].
    pub fn write_stdin_pipe(&self, bytes: &[u8]) -> Result<()> {
        let mut guard = self
            .stdin_pipe
            .lock()
            .map_err(|e| anyhow!("stdin_pipe poisoned: {e}"))?;
        let Some(pipe) = guard.as_mut() else {
            anyhow::bail!(
                "no stdin pipe — pane was spawned with stdin_mode={:?}; use `press` / `type` instead",
                self.stdin_mode
            );
        };
        pipe.write_all(bytes).context("write to stdin pipe")?;
        pipe.flush().context("flush stdin pipe")?;
        Ok(())
    }

    /// Close the child's stdin pipe (when spawned with `StdinMode::Pipe`).
    /// EOF on the child. No-op for `Pty` / `Closed`.
    pub fn close_stdin_pipe(&self) -> Result<()> {
        let mut guard = self
            .stdin_pipe
            .lock()
            .map_err(|e| anyhow!("stdin_pipe poisoned: {e}"))?;
        // Dropping the File closes the write end of the pipe; the child's
        // read(stdin) returns 0 (EOF).
        let had_pipe = guard.is_some();
        *guard = None;
        tracing::debug!(had_pipe, "close_stdin_pipe dropped stdin writer");
        Ok(())
    }

    /// Which stdin mode was this pane spawned with.
    #[must_use]
    pub fn stdin_mode(&self) -> StdinMode {
        self.stdin_mode
    }

    /// Write bytes to the PTY master end (delivered to the child as input).
    /// Also tees an `i` event to the attached recorder, if any.
    pub fn write_input(&self, bytes: &[u8]) -> Result<()> {
        let mut w = self
            .writer
            .lock()
            .map_err(|e| anyhow!("writer poisoned: {e}"))?;
        w.write_all(bytes).context("write to pty")?;
        w.flush().context("flush pty")?;
        drop(w);
        if let Some(rec) = &self.recorder {
            rec.push_input(Some(bytes));
        }
        Ok(())
    }

    /// Inform the kernel of a new window size; the child receives SIGWINCH.
    /// Tees an `r` event to the attached recorder, if any.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        match &self.master {
            MasterEnd::Portable(m) => {
                let m = m.lock().map_err(|e| anyhow!("master poisoned: {e}"))?;
                m.resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .context("pty resize")?;
            }
            #[cfg(unix)]
            MasterEnd::Adopted(fd) => {
                use std::os::fd::AsRawFd;
                let fd = fd.lock().map_err(|e| anyhow!("master poisoned: {e}"))?;
                let ws = libc::winsize {
                    ws_row: rows,
                    ws_col: cols,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };
                #[allow(unsafe_code)]
                let rc = unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
                if rc == -1 {
                    return Err(std::io::Error::last_os_error()).context("pty resize (TIOCSWINSZ)");
                }
            }
        }
        if let Some(rec) = &self.recorder {
            rec.push_resize(cols, rows);
        }
        Ok(())
    }

    /// Poll the child without blocking. Returns the exit code if the child
    /// has terminated.
    pub fn try_exit_code(&self) -> Result<Option<u32>> {
        match &self.child {
            ChildEnd::Portable { child, .. } => {
                let mut c = child.lock().map_err(|e| anyhow!("child poisoned: {e}"))?;
                Ok(c.try_wait()?.map(|s| shell_exit_code(&s)))
            }
            #[cfg(unix)]
            ChildEnd::Adopted { pid, exited } => {
                let mut slot = exited.lock().map_err(|e| anyhow!("exited poisoned: {e}"))?;
                if let Some(code) = *slot {
                    return Ok(Some(code));
                }
                let code = adopted_try_wait(*pid)?;
                if let Some(c) = code {
                    *slot = Some(c);
                }
                Ok(code)
            }
        }
    }

    /// Forcibly terminate the child. For an adopted child we SIGKILL the pid
    /// directly (the daemon is still its parent post-re-exec).
    pub fn kill(&self) -> Result<()> {
        match &self.child {
            ChildEnd::Portable { killer, .. } => {
                let mut k = killer.lock().map_err(|e| anyhow!("killer poisoned: {e}"))?;
                k.kill().context("kill child")?;
                Ok(())
            }
            #[cfg(unix)]
            ChildEnd::Adopted { pid, .. } => {
                nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(*pid),
                    nix::sys::signal::Signal::SIGKILL,
                )
                .context("kill adopted child")?;
                Ok(())
            }
        }
    }

    /// Process-group leader pid for `signal` delivery.
    #[cfg(unix)]
    pub fn pgid(&self) -> Option<i32> {
        match &self.master {
            MasterEnd::Portable(m) => m.lock().ok()?.process_group_leader(),
            // `tcgetpgrp(master)` returns the controlling terminal's
            // foreground process group — the child's pgid (it did `setsid` +
            // TIOCSCTTY at spawn). Works identically for an adopted master.
            MasterEnd::Adopted(fd) => {
                use std::os::fd::{AsRawFd, BorrowedFd};
                let fd = fd.lock().ok()?;
                #[allow(unsafe_code)]
                let borrowed = unsafe { BorrowedFd::borrow_raw(fd.as_raw_fd()) };
                nix::unistd::tcgetpgrp(borrowed)
                    .ok()
                    .map(nix::unistd::Pid::as_raw)
            }
        }
    }

    /// Child PID for Windows `GenerateConsoleCtrlEvent` delivery. portable-pty
    /// spawns the child with `CREATE_NEW_PROCESS_GROUP` so the PID doubles as
    /// the process-group id Windows control events expect.
    pub fn child_pid(&self) -> Option<u32> {
        match &self.child {
            ChildEnd::Portable { child, .. } => child.lock().ok()?.process_id(),
            #[cfg(unix)]
            #[allow(clippy::cast_sign_loss)]
            ChildEnd::Adopted { pid, .. } => Some(*pid as u32),
        }
    }

    /// Raw master-PTY fd, for handing off across an in-place re-exec
    /// (`daemon upgrade`). `None` on a non-Unix platform or if the lock is
    /// poisoned.
    #[cfg(unix)]
    pub fn raw_master_fd(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        match &self.master {
            MasterEnd::Portable(m) => m.lock().ok()?.as_raw_fd(),
            MasterEnd::Adopted(fd) => Some(fd.lock().ok()?.as_raw_fd()),
        }
    }

    /// Current output high-water mark (cumulative bytes ever observed). Used
    /// by the upgrade handoff to record where the new image should consider
    /// the `.cast` replay caught up to.
    #[must_use]
    pub fn output_total(&self) -> u64 {
        self.output_buf.lock().map_or(0, |g| g.total)
    }

    /// Borrowed reference to the attached recorder, if any. Used by
    /// the dispatch tap to emit `m` (tool-call) events.
    pub fn recorder(&self) -> Option<&Recorder> {
        self.recorder.as_ref()
    }

    /// Snapshot the first ~512 PTY-output bytes captured so far. Used by
    /// the spawn handler's deferred adapter re-detection pass.
    pub fn first_bytes_snapshot(&self) -> Vec<u8> {
        self.first_bytes
            .lock()
            .map_or_else(|_| Vec::new(), |fb| fb.buf.clone())
    }

    /// Whether the first-bytes buffer has filled (or the child exited
    /// without producing 512 bytes — `done` is set when EOF is observed).
    pub fn first_bytes_done(&self) -> bool {
        self.first_bytes.lock().map_or(true, |fb| fb.done)
    }

    /// Most recent OSC 133 marker observed on this PTY, if any.
    pub fn last_osc133_marker(&self) -> Option<crate::osc133::Marker> {
        self.last_osc133.lock().ok().and_then(|m| *m)
    }

    /// Whether the reader task has observed EOF on the PTY master and
    /// exited. Once true, the output ring holds every byte the child ever
    /// wrote — the streaming `tail --follow` path uses this to avoid
    /// emitting `eof` (and dropping trailing output) while bytes are still
    /// in flight from a child that `try_wait` already reports as exited.
    #[must_use]
    pub fn reader_finished(&self) -> bool {
        self.reader_done.load(Ordering::Acquire)
    }

    /// Subscribe to output-append notifications. The returned `watch::Receiver`
    /// observes the output ring's high-water mark and fires `changed()` every
    /// time the reader task appends a new chunk. Streaming followers
    /// (`attach`, `tail --follow`) block on this instead of polling on a fixed
    /// tick, so an echoed keystroke reaches the follower as soon as the child
    /// produces it rather than at the next periodic flush.
    #[must_use]
    pub fn subscribe_output(&self) -> watch::Receiver<u64> {
        self.output_tx.subscribe()
    }
}

impl Drop for PtyChild {
    fn drop(&mut self) {
        if let Ok(mut handle_slot) = self.reader.lock()
            && let Some(handle) = handle_slot.take()
        {
            handle.abort();
        }
    }
}

/// Spawn a child with stdout/stderr wired to the slave PTY but stdin
/// redirected per [`StdinMode`]. Replicates the bits of
/// `portable-pty`'s own `spawn_command` that we can't override (setsid,
/// TIOCSCTTY, signal-mask reset) inside our own `pre_exec`.
///
/// Returns the child + (when mode is `Pipe`) the write end of the
/// stdin pipe so [`PtyChild`] can route `write_stdin_pipe` bytes to it.
///
/// **Slave fd acquisition.** portable-pty's `SlavePty` trait doesn't
/// expose `as_raw_fd()`, but the master end does, and POSIX `ptsname()`
/// of the master returns the slave's pty device path. We open it ourselves
/// (`O_NOCTTY` so the open doesn't accidentally make it our ctty), then
/// drop the portable-pty-owned slave so we own the only remaining handle.
#[cfg(unix)]
fn spawn_with_custom_stdin(
    argv: &[String],
    cwd: Option<&Path>,
    env: &[(String, String)],
    master: &dyn portable_pty::MasterPty,
    stdin_mode: StdinMode,
) -> Result<(Box<dyn Child + Send + Sync>, Option<std::fs::File>)> {
    use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let master_fd = master
        .as_raw_fd()
        .ok_or_else(|| anyhow!("master PTY has no raw fd (non-Unix platform?)"))?;

    // Resolve the slave path from the master fd via `ptsname_r`.
    let slave_path = ptsname_owned(master_fd)?;

    // Open the slave ourselves. O_NOCTTY so this open doesn't promote
    // to ctty for the daemon process; we'll TIOCSCTTY in the child's
    // pre_exec.
    let slave_owned = {
        #[allow(unsafe_code)]
        let fd = unsafe { libc::open(slave_path.as_ptr().cast(), libc::O_RDWR | libc::O_NOCTTY) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("open slave PTY");
        }
        #[allow(unsafe_code)]
        unsafe {
            OwnedFd::from_raw_fd(fd)
        }
    };

    // We need the slave on both stdout and stderr; dup so each Stdio
    // owns its own file descriptor (Command will close them after exec).
    let slave_out = dup_owned(&slave_owned)?;
    let slave_err = dup_owned(&slave_owned)?;

    // For Pipe mode: a pipe whose ends are close-on-exec, so they
    // DON'T leak into the child via inherited fds — a critical
    // detail, because if the child inherits the write end, closing
    // our daemon's write fd doesn't EOF the read end (the child
    // holds it open against itself). The stdin Stdio's fd is
    // exempted from CLOEXEC by std::process during dup2 → child
    // fd 0, which is the right behavior.
    //
    // `pipe2(O_CLOEXEC)` is the one-syscall path on Linux but isn't
    // available on macOS. `cloexec_pipe()` below picks the right
    // implementation per target.
    let (stdin_pipe_writer, stdin_stdio): (Option<std::fs::File>, Stdio) = match stdin_mode {
        StdinMode::Pipe => {
            let (read_end, write_end) = cloexec_pipe()?;
            (
                Some(std::fs::File::from(write_end)),
                Stdio::from(std::fs::File::from(read_end)),
            )
        }
        StdinMode::Closed => (None, Stdio::null()),
        StdinMode::Pty => unreachable!("Pty stdin doesn't use the custom path"),
    };

    let mut cmd = std::process::Command::new(&argv[0]);
    for arg in &argv[1..] {
        cmd.arg(arg);
    }
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.stdin(stdin_stdio);
    // Convert OwnedFd → Stdio via raw fd (only way std::process
    // accepts).
    #[allow(unsafe_code)]
    {
        cmd.stdout(unsafe { Stdio::from_raw_fd(slave_out.into_raw_fd()) });
        cmd.stderr(unsafe { Stdio::from_raw_fd(slave_err.into_raw_fd()) });
    }
    // Replicate portable-pty's pre_exec: signal reset + setsid +
    // TIOCSCTTY. After `fork()` the child can only call async-signal-
    // safe syscalls; raw libc calls are appropriate here.
    #[allow(unsafe_code)]
    unsafe {
        cmd.pre_exec(move || {
            for signo in &[
                libc::SIGCHLD,
                libc::SIGHUP,
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTERM,
                libc::SIGALRM,
            ] {
                libc::signal(*signo, libc::SIG_DFL);
            }
            let empty: libc::sigset_t = std::mem::zeroed();
            libc::sigprocmask(
                libc::SIG_SETMASK,
                std::ptr::from_ref(&empty),
                std::ptr::null_mut(),
            );
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // fd 1 (stdout) is the slave PTY in the child by now;
            // TIOCSCTTY on it makes the PTY the child's ctty.
            #[allow(clippy::cast_lossless)]
            if libc::ioctl(1, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd.spawn().context("spawn child (custom stdin)")?;
    // After spawn the child has its own dup of slave_out/_err; the
    // copies we held (`slave_owned`) drop here, closing the only
    // remaining parent-side handle.
    drop(slave_owned);
    let boxed: Box<dyn Child + Send + Sync> = Box::new(StdChildShim::new(child));
    Ok((boxed, stdin_pipe_writer))
}

/// Allocate a pipe with both ends marked close-on-exec.
///
/// `pipe2(O_CLOEXEC)` would be the single-syscall path but rustix
/// (and libc) decline to provide it on Apple — macOS doesn't have a
/// `pipe2` syscall. The portable path uses `pipe()` + `fcntl_setfd(
/// CLOEXEC)` on each end. Non-atomic, but the daemon is
/// single-threaded at spawn time, so a concurrent fork can't leak
/// the fds in the window between calls.
#[cfg(unix)]
fn cloexec_pipe() -> Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    let (r, w) = rustix::pipe::pipe().context("pipe()")?;
    rustix::io::fcntl_setfd(&r, rustix::io::FdFlags::CLOEXEC)
        .context("fcntl_setfd(CLOEXEC) on read end")?;
    rustix::io::fcntl_setfd(&w, rustix::io::FdFlags::CLOEXEC)
        .context("fcntl_setfd(CLOEXEC) on write end")?;
    Ok((r, w))
}

/// Read the slave PTY's device path from a master fd.
/// Resolve the slave PTY device path from the master fd.
///
/// `rustix::pty::ptsname` is cross-platform: it picks `ptsname_r` on
/// Linux and a safe wrapper around the global-buffer `ptsname` on
/// macOS, returning a fresh `CString` either way. We hand it a
/// `BorrowedFd` made from the master raw fd.
#[cfg(unix)]
fn ptsname_owned(master_fd: i32) -> Result<std::ffi::CString> {
    #[allow(unsafe_code)]
    let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(master_fd) };
    rustix::pty::ptsname(borrowed, Vec::new()).context("ptsname")
}

/// Non-blocking `waitpid(WNOHANG)` on an adopted child, mapping the status to
/// a shell-style code (signal death → 128+sig). Returns `None` while the child
/// is still alive (or already reaped elsewhere — `ECHILD`).
#[cfg(unix)]
fn adopted_try_wait(pid: i32) -> Result<Option<u32>> {
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;
    match waitpid(Pid::from_raw(pid), Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::Exited(_, code)) => Ok(Some(u32::try_from(code).unwrap_or(1))),
        #[allow(clippy::cast_sign_loss)]
        Ok(WaitStatus::Signaled(_, sig, _)) => Ok(Some(128 + (sig as i32) as u32)),
        // StillAlive / Stopped / Continued / Ptrace* → not terminal; or
        // ECHILD → already reaped elsewhere / never our child after an exotic
        // restart. Either way there's no observable terminal exit to report.
        Ok(_) | Err(nix::errno::Errno::ECHILD) => Ok(None),
        Err(e) => Err(anyhow!("waitpid({pid}): {e}")),
    }
}

/// Clear `O_NONBLOCK` on `fd` (best-effort). The adopted reader loop does
/// blocking reads; an inherited fd left in non-blocking mode would spin.
#[cfg(unix)]
fn clear_nonblocking(fd: std::os::fd::RawFd) {
    #[allow(unsafe_code)]
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
        }
    }
}

#[cfg(unix)]
fn dup_owned(src: &std::os::fd::OwnedFd) -> Result<std::os::fd::OwnedFd> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    // F_DUPFD_CLOEXEC atomically dups + sets CLOEXEC. Critical so the
    // duped slave fds don't leak into the child via fd-inheritance:
    // if the child inherits an open slave-PTY fd at some unrelated
    // number, closing one end of the PTY doesn't propagate EOF (the
    // child holds its own copy open).
    #[allow(unsafe_code)]
    let dup = unsafe { libc::fcntl(src.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if dup < 0 {
        return Err(std::io::Error::last_os_error()).context("dup slave");
    }
    #[allow(unsafe_code)]
    Ok(unsafe { OwnedFd::from_raw_fd(dup) })
}

/// Wraps a `std::process::Child` so it satisfies portable-pty's `Child`
/// + `ChildKiller` traits. Used only by the custom-stdin spawn path.
#[cfg(unix)]
#[derive(Debug)]
struct StdChildShim {
    child: std::sync::Arc<std::sync::Mutex<std::process::Child>>,
}

#[cfg(unix)]
impl StdChildShim {
    fn new(c: std::process::Child) -> Self {
        Self {
            child: std::sync::Arc::new(std::sync::Mutex::new(c)),
        }
    }
}

#[cfg(unix)]
impl portable_pty::Child for StdChildShim {
    fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
        let mut g = self.child.lock().expect("child mutex poisoned");
        match g.try_wait()? {
            Some(status) => Ok(Some(map_exit(status))),
            None => Ok(None),
        }
    }
    fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
        let mut g = self.child.lock().expect("child mutex poisoned");
        Ok(map_exit(g.wait()?))
    }
    fn process_id(&self) -> Option<u32> {
        let g = self.child.lock().expect("child mutex poisoned");
        Some(g.id())
    }
}

#[cfg(unix)]
impl portable_pty::ChildKiller for StdChildShim {
    fn kill(&mut self) -> std::io::Result<()> {
        let mut g = self.child.lock().expect("child mutex poisoned");
        g.kill()
    }
    fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
        Box::new(Self {
            child: self.child.clone(),
        })
    }
}

#[cfg(unix)]
fn map_exit(status: std::process::ExitStatus) -> portable_pty::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        #[allow(clippy::cast_sign_loss)]
        portable_pty::ExitStatus::with_exit_code(code as u32)
    } else if let Some(sig) = status.signal() {
        #[allow(clippy::cast_sign_loss)]
        portable_pty::ExitStatus::with_exit_code(128u32 + sig as u32)
    } else {
        portable_pty::ExitStatus::with_exit_code(1)
    }
}

/// Map a portable-pty `ExitStatus` to a **shell-style** code: a normal exit
/// keeps its code; a signal death becomes 128 + signal. portable-pty's own unix
/// child reports a signal death via `.signal()` (a `strsignal` *name*) while
/// collapsing `.exit_code()` to 1, so we reverse the name to a number using the
/// same `strsignal` table (locale-independent: it round-trips whatever the
/// platform produced). The `StdChildShim` path already encodes 128+sig in
/// `exit_code()` with no signal name, so it falls through unchanged. This is the
/// one place `try_exit_code` derives the faithful code for every spawn path.
fn shell_exit_code(status: &portable_pty::ExitStatus) -> u32 {
    if let Some(name) = status.signal()
        && let Some(sig) = signal_name_to_num(name)
    {
        return 128 + sig;
    }
    status.exit_code()
}

/// Reverse a `strsignal` name to its signal number via the same table that
/// produced it. Returns `None` for an unknown name (or on non-unix).
#[cfg(unix)]
fn signal_name_to_num(name: &str) -> Option<u32> {
    for sig in 1..=31i32 {
        #[allow(unsafe_code)]
        let ptr = unsafe { libc::strsignal(sig) };
        if ptr.is_null() {
            continue;
        }
        #[allow(unsafe_code)]
        let s = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_string_lossy();
        if s == name {
            return u32::try_from(sig).ok();
        }
    }
    None
}

#[cfg(not(unix))]
fn signal_name_to_num(_name: &str) -> Option<u32> {
    None
}

#[allow(clippy::too_many_arguments)]
fn pty_reader_loop(
    mut reader: Box<dyn Read + Send>,
    engine: &Arc<dyn Engine>,
    recorder: Option<&Recorder>,
    first_bytes: &Arc<Mutex<FirstBytes>>,
    last_osc133: &Arc<Mutex<Option<crate::osc133::Marker>>>,
    output_buf: &Arc<Mutex<OutputRing>>,
    capture_lock: &Arc<Mutex<()>>,
    output_tx: &watch::Sender<u64>,
) {
    let mut buf = [0u8; 8192];
    let mut osc_scanner = crate::osc133::Scanner::new();
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                if let Ok(mut fb) = first_bytes.lock() {
                    fb.done = true;
                }
                break;
            }
            Ok(n) => {
                // Feed the engine and push to the ring as one unit under the
                // capture lock, so an `attach` capture observes a consistent
                // (rendered frame, high-water) pair — no byte falls between.
                let mut new_total: Option<u64> = None;
                let feed_result = {
                    let Ok(_cap) = capture_lock.lock() else {
                        break;
                    };
                    let r = engine.feed(&buf[..n]);
                    if r.is_ok()
                        && let Ok(mut ring) = output_buf.lock()
                    {
                        ring.push(&buf[..n]);
                        new_total = Some(ring.total);
                    }
                    r
                };
                if let Err(e) = feed_result {
                    tracing::warn!(error = %e, "engine.feed failed; ending pty reader");
                    break;
                }
                // Wake any streaming followers as soon as the bytes are in the
                // ring (outside the capture lock). `send_replace` never errors,
                // even with zero current subscribers.
                if let Some(total) = new_total {
                    output_tx.send_replace(total);
                }
                if let Some(rec) = recorder {
                    rec.push_output(&buf[..n]);
                }
                // OSC 133 scanning is independent of the engine — we look at
                // raw bytes so missing shell-prompt support in the VT parser
                // doesn't bleed through to the classifier.
                let markers = osc_scanner.feed(&buf[..n]);
                if let Some(last) = markers.last()
                    && let Ok(mut slot) = last_osc133.lock()
                {
                    *slot = Some(*last);
                }
                if let Ok(mut fb) = first_bytes.lock()
                    && !fb.done
                {
                    let remaining = MAX_FIRST_BYTES.saturating_sub(fb.buf.len());
                    let take = remaining.min(n);
                    fb.buf.extend_from_slice(&buf[..take]);
                    if fb.buf.len() >= MAX_FIRST_BYTES {
                        fb.done = true;
                    }
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "pty read ended");
                if let Ok(mut fb) = first_bytes.lock() {
                    fb.done = true;
                }
                break;
            }
        }
    }
}
