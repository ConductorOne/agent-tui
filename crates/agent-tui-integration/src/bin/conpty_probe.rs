//! Windows-only test probes for the ConPTY paths shipped in PR #124.
//!
//! Two modes, used by `tests/windows_conpty.rs`:
//!
//! `conpty_probe dsr`
//!   The minimal interactive child that blocks on the terminal's answer to the
//!   startup DSR (`ESC[6n` → `ESC[<row>;<col>R`). Enables VT input on its own
//!   console (so the reply arrives as raw bytes, the way a real TUI reads it),
//!   emits the query, reads the reply, and prints it as `DSR-REPLY:<bytes>`
//!   with `DSR-READ:` diagnostics. Without the daemon's `take_pty_writes`
//!   write-back the read never produces the reply.
//!
//! `conpty_probe read-stdin`
//!   Reads up to 16 stdin bytes (stopping after `0x03`) and prints them as
//!   `STDIN-HEX:<hex>`. Proves the daemon's ConPTY-input write path
//!   (`signal SIGINT` writes ETX) end to end, independent of conhost's
//!   mode-dependent ETX → `CTRL_C_EVENT` translation.
//!
//! Built as a bin of the integration crate so `cargo test` places it next to
//! `agent-tui.exe` under `target/debug/` on the Windows CI leg. On non-Windows
//! it compiles to an inert stub so the workspace builds everywhere.

#![deny(unsafe_code)]

#[cfg(windows)]
mod imp {
    use std::fmt::Write as _;
    use std::io::{Read, Write};

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
    };

    /// `ENABLE_VIRTUAL_TERMINAL_INPUT` (0x0200): deliver VT sequences — including
    /// terminal replies and ETX — as raw input bytes instead of cooked key
    /// events or processed-input control events.
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

    /// Enable VT input on our own console. Returns false (with a printed
    /// diagnostic) if any step fails — on a ConPTY client these handles are
    /// console handles, so failure here is itself interesting test data.
    fn enable_vt_input() -> bool {
        // SAFETY: standard Win32 console calls on our own process's stdin
        // handle; the returned handle is borrowed, never closed here.
        #[allow(unsafe_code)]
        unsafe {
            let h: HANDLE = GetStdHandle(STD_INPUT_HANDLE);
            if h.is_null() {
                println!("MODE-SET:fail:null-handle");
                return false;
            }
            let mut mode: u32 = 0;
            if GetConsoleMode(h, &raw mut mode) == 0 {
                println!("MODE-SET:fail:get-mode:{}", std::io::Error::last_os_error());
                return false;
            }
            if SetConsoleMode(h, ENABLE_VIRTUAL_TERMINAL_INPUT) == 0 {
                println!("MODE-SET:fail:set-mode:{}", std::io::Error::last_os_error());
                return false;
            }
            println!("MODE-SET:ok:was-{mode:#x}");
            true
        }
    }

    /// Read up to `buf.len()` bytes, stopping early at `stop` or EOF/error.
    /// Returns (bytes-read, status string) for diagnostics.
    fn read_until(buf: &mut [u8], stop: u8) -> (usize, String) {
        let mut n = 0usize;
        let stdin = std::io::stdin();
        let mut lock = stdin.lock();
        while n < buf.len() {
            match lock.read(&mut buf[n..=n]) {
                Ok(0) => return (n, "eof".to_string()),
                Ok(m) => {
                    n += m;
                    if buf[n - 1] == stop {
                        return (n, "stop-byte".to_string());
                    }
                }
                Err(e) => return (n, format!("err:{e}")),
            }
        }
        (n, "full".to_string())
    }

    fn dsr() -> i32 {
        if !enable_vt_input() {
            return 2;
        }
        print!("\x1b[6n");
        let _ = std::io::stdout().flush();
        let mut buf = [0u8; 64];
        let (n, status) = read_until(&mut buf, b'R');
        println!("DSR-READ:n={n}:status={status}");
        // Print the reply as HEX, not raw: the reply bytes are themselves a VT
        // sequence (ESC[<row>;<col>R) and the pane's own terminal parser
        // swallows them — which is exactly what hid a *successful* reply on
        // the first CI iteration (n=6, stop-byte, empty text).
        let mut hex = String::with_capacity(n * 2);
        for b in &buf[..n] {
            let _ = write!(hex, "{b:02x}");
        }
        println!("DSR-REPLY-HEX:{hex}");
        0
    }

    fn read_stdin() -> i32 {
        if !enable_vt_input() {
            return 2;
        }
        let mut buf = [0u8; 16];
        let (n, status) = read_until(&mut buf, 0x03);
        let mut hex = String::with_capacity(n * 2);
        for b in &buf[..n] {
            let _ = write!(hex, "{b:02x}");
        }
        println!("STDIN-READ:n={n}:status={status}");
        println!("STDIN-HEX:{hex}");
        0
    }

    pub(crate) fn run() -> i32 {
        match std::env::args().nth(1).as_deref() {
            Some("dsr") => dsr(),
            Some("read-stdin") => read_stdin(),
            other => {
                eprintln!("usage: conpty_probe <dsr|read-stdin>, got {other:?}");
                2
            }
        }
    }
}

fn main() {
    #[cfg(windows)]
    std::process::exit(imp::run());
    #[cfg(not(windows))]
    {
        eprintln!("conpty_probe is a Windows-only test probe");
        std::process::exit(2);
    }
}
