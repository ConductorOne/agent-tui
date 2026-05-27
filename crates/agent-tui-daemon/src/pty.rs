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
use std::sync::{Arc, Mutex};

use agent_tui_engine::Engine;
use agent_tui_protocol::request::StdinMode;
use agent_tui_recorder::Recorder;
use anyhow::{Context, Result, anyhow};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::task::JoinHandle;

/// A spawned PTY child, paired with the engine that consumes its output.
///
/// Every internal handle that `portable-pty` returns is `Send` but **not**
/// `Sync` (no shared-reference invariants are guaranteed by the underlying
/// fd-owning types), so we wrap each in a `Mutex` to make the whole struct
/// `Send + Sync` for storage inside `Arc<Pane>`.
pub struct PtyChild {
    master: Mutex<Box<dyn MasterPty + Send>>,
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
    child: Mutex<Box<dyn Child + Send + Sync>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    /// Reader task handle; kept so callers can abort/await on drop.
    reader: Mutex<Option<JoinHandle<()>>>,
    /// Optional recorder so input events can be teed back into the cast log.
    recorder: Option<Recorder>,
    /// First ~512 bytes seen from the PTY. Used for late adapter re-detection.
    first_bytes: Arc<Mutex<FirstBytes>>,
    /// Most recent OSC 133 marker seen on this PTY, used by the shell-state
    /// classifier. Updated by the reader task; read by the snapshot handler.
    last_osc133: Arc<Mutex<Option<crate::osc133::Marker>>>,
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
    #[allow(clippy::too_many_arguments)]
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
        // Final env list: LINES/COLUMNS first (so a user `env` of
        // `LINES=foo` can deliberately override our default), then the
        // caller's per-spawn overrides. portable-pty's CommandBuilder
        // applies each `cmd.env(k, v)` in iteration order, so later
        // entries win.
        let mut env_overrides: Vec<(String, String)> = vec![
            ("LINES".to_string(), rows.to_string()),
            ("COLUMNS".to_string(), cols.to_string()),
        ];
        env_overrides.extend(env.iter().cloned());

        let (child, stdin_pipe): (Box<dyn Child + Send + Sync>, Option<std::fs::File>) =
            match stdin_mode {
                StdinMode::Pty => {
                    // Existing behavior: portable-pty spawn, slave PTY on all
                    // three FDs.
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
        let reader_handle = tokio::task::spawn_blocking(move || {
            pty_reader_loop(
                reader,
                &reader_engine,
                reader_recorder.as_ref(),
                &reader_first_bytes,
                &reader_osc133,
                &reader_output_buf,
            );
        });

        Ok(Self {
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            stdin_pipe: Mutex::new(stdin_pipe),
            stdin_mode,
            output_buf,
            child: Mutex::new(child),
            killer: Mutex::new(killer),
            reader: Mutex::new(Some(reader_handle)),
            recorder,
            first_bytes,
            last_osc133,
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
        let m = self
            .master
            .lock()
            .map_err(|e| anyhow!("master poisoned: {e}"))?;
        m.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("pty resize")?;
        drop(m);
        if let Some(rec) = &self.recorder {
            rec.push_resize(cols, rows);
        }
        Ok(())
    }

    /// Poll the child without blocking. Returns the exit code if the child
    /// has terminated.
    pub fn try_exit_code(&self) -> Result<Option<u32>> {
        let mut c = self
            .child
            .lock()
            .map_err(|e| anyhow!("child poisoned: {e}"))?;
        Ok(c.try_wait()?.map(|s| s.exit_code()))
    }

    /// Forcibly terminate the child via the killer handle.
    pub fn kill(&self) -> Result<()> {
        let mut k = self
            .killer
            .lock()
            .map_err(|e| anyhow!("killer poisoned: {e}"))?;
        k.kill().context("kill child")?;
        Ok(())
    }

    /// Process-group leader pid for `signal` delivery.
    #[cfg(unix)]
    pub fn pgid(&self) -> Option<i32> {
        let m = self.master.lock().ok()?;
        m.process_group_leader()
    }

    /// Child PID for Windows `GenerateConsoleCtrlEvent` delivery. portable-pty
    /// spawns the child with `CREATE_NEW_PROCESS_GROUP` so the PID doubles as
    /// the process-group id Windows control events expect.
    pub fn child_pid(&self) -> Option<u32> {
        let c = self.child.lock().ok()?;
        c.process_id()
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
/// `rustix::pipe::pipe_with(CLOEXEC)` is a single atomic syscall on
/// every Unix target — `pipe2(O_CLOEXEC)` on Linux/BSD, and the
/// `pipe()` + `fcntl(FD_CLOEXEC)` emulation on macOS. The wrapper
/// hides the cfg-split that bit us in PR #2.
#[cfg(unix)]
fn cloexec_pipe() -> Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).context("pipe_with(CLOEXEC)")
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

fn pty_reader_loop(
    mut reader: Box<dyn Read + Send>,
    engine: &Arc<dyn Engine>,
    recorder: Option<&Recorder>,
    first_bytes: &Arc<Mutex<FirstBytes>>,
    last_osc133: &Arc<Mutex<Option<crate::osc133::Marker>>>,
    output_buf: &Arc<Mutex<OutputRing>>,
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
                if let Err(e) = engine.feed(&buf[..n]) {
                    tracing::warn!(error = %e, "engine.feed failed; ending pty reader");
                    break;
                }
                if let Some(rec) = recorder {
                    rec.push_output(&buf[..n]);
                }
                if let Ok(mut ring) = output_buf.lock() {
                    ring.push(&buf[..n]);
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
