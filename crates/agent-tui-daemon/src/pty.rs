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
    child: Mutex<Box<dyn Child + Send + Sync>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    /// Reader task handle; kept so callers can abort/await on drop.
    reader: Mutex<Option<JoinHandle<()>>>,
    /// Optional recorder so input events can be teed back into the cast log.
    recorder: Option<Recorder>,
}

impl PtyChild {
    /// Spawn `argv` under a fresh PTY of size `(cols, rows)` and start the
    /// reader task piping output into `engine.feed`. `recorder`, when
    /// supplied, gets a tee of every byte chunk read from the PTY.
    pub fn spawn(
        argv: &[String],
        cwd: Option<&Path>,
        cols: u16,
        rows: u16,
        engine: Arc<dyn Engine>,
        recorder: Option<Recorder>,
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

        let mut cmd = CommandBuilder::new(&argv[0]);
        for arg in &argv[1..] {
            cmd.arg(arg);
        }
        if let Some(d) = cwd {
            cmd.cwd(d);
        }

        let child = pair.slave.spawn_command(cmd).context("spawn child")?;
        let killer = child.clone_killer();

        // Drop the slave end so the kernel closes it on child exit and the
        // master read loop sees EOF instead of hanging.
        drop(pair.slave);

        let writer = pair.master.take_writer().context("take_writer")?;
        let reader = pair.master.try_clone_reader().context("clone_reader")?;

        let reader_engine = engine;
        let reader_recorder = recorder.clone();
        let reader_handle = tokio::task::spawn_blocking(move || {
            pty_reader_loop(reader, &reader_engine, reader_recorder.as_ref());
        });

        Ok(Self {
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            killer: Mutex::new(killer),
            reader: Mutex::new(Some(reader_handle)),
            recorder,
        })
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

fn pty_reader_loop(
    mut reader: Box<dyn Read + Send>,
    engine: &Arc<dyn Engine>,
    recorder: Option<&Recorder>,
) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Err(e) = engine.feed(&buf[..n]) {
                    tracing::warn!(error = %e, "engine.feed failed; ending pty reader");
                    break;
                }
                if let Some(rec) = recorder {
                    rec.push_output(&buf[..n]);
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "pty read ended");
                break;
            }
        }
    }
}
