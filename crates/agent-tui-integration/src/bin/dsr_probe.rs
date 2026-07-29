//! Windows-only test probe for the ConPTY DSR (Device Status Report) write-back
//! path shipped in PR #124.
//!
//! Interactive programs emit `ESC[6n` at startup and block until the terminal
//! answers with a cursor-position report (`ESC[<row>;<col>R`). The daemon's
//! engine produces that reply (`Event::PtyWrite` → `take_pty_writes`) and the
//! reader loop writes it back to the ConPTY master. This probe is the minimal
//! such child: it enables VT input on its own console (so the reply arrives as
//! raw bytes, the way a real TUI reads it), emits `ESC[6n`, reads the reply,
//! and prints it as `DSR-REPLY:<bytes>`. The companion test
//! (`tests/windows_conpty.rs::interactive_pane_answers_startup_dsr`) asserts
//! the reply arrives; without the write-back the read below blocks forever.
//!
//! Built as a bin of the integration crate so `cargo test` places it next to
//! `agent-tui.exe` under `target/debug/` on the Windows CI leg. On non-Windows
//! it compiles to an inert stub so the workspace builds everywhere.

#![deny(unsafe_code)]

#[cfg(windows)]
mod imp {
    use std::io::{Read, Write};

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
    };

    /// `ENABLE_VIRTUAL_TERMINAL_INPUT` (0x0200): deliver VT sequences — including
    /// the terminal's DSR reply — as raw input bytes instead of cooked key events.
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

    pub(crate) fn run() -> i32 {
        // SAFETY: standard Win32 console calls on our own process's stdin
        // handle; the returned handle is borrowed, never closed here.
        #[allow(unsafe_code)]
        unsafe {
            let h: HANDLE = GetStdHandle(STD_INPUT_HANDLE);
            if h.is_null() {
                return 2;
            }
            let mut mode: u32 = 0;
            if GetConsoleMode(h, &raw mut mode) == 0 {
                return 2;
            }
            // VT input only: no line buffering, no echo of the reply bytes.
            if SetConsoleMode(h, ENABLE_VIRTUAL_TERMINAL_INPUT) == 0 {
                return 2;
            }
        }

        print!("\x1b[6n");
        let _ = std::io::stdout().flush();

        let mut buf = [0u8; 64];
        let mut n = 0usize;
        let stdin = std::io::stdin();
        let mut lock = stdin.lock();
        while n < buf.len() {
            match lock.read(&mut buf[n..=n]) {
                Ok(0) | Err(_) => break,
                Ok(m) => {
                    n += m;
                    if buf[n - 1] == b'R' {
                        break;
                    }
                }
            }
        }
        println!("DSR-REPLY:{}", String::from_utf8_lossy(&buf[..n]));
        0
    }
}

fn main() {
    #[cfg(windows)]
    std::process::exit(imp::run());
    #[cfg(not(windows))]
    {
        eprintln!("dsr_probe is a Windows-only test probe");
        std::process::exit(2);
    }
}
