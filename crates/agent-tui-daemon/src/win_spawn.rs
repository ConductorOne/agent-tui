//! Windows-only: spawn the lazily-started daemon so it inherits **no** handles
//! except three fresh `NUL` std handles.
//!
//! ## Why this exists
//!
//! The CLI lazily spawns the DETACHED daemon with `CreateProcess`. Rust's std
//! `Command` (and the previous std-handle-inheritance shim) leave
//! `bInheritHandles = TRUE`, so the daemon inherits *every* inheritable handle
//! the client process holds. Stripping `HANDLE_FLAG_INHERIT` from the client's
//! own three std handles is **not enough**: when the client is launched inside a
//! `cmd.exe` pipeline (`agent-tui run … | consumer`), `cmd` creates the pipe
//! with inheritable handles and hands the client an *extra* inheritable pipe
//! handle beyond its own stdout. The long-lived daemon then inherits and holds
//! that pipe's write end, so the consumer never sees EOF and blocks until the
//! daemon's idle-timeout (minutes) — a real hang (`run | findstr` etc.).
//!
//! A direct (non-`cmd`) launch does not hit this because .NET / the parent uses
//! `STARTUPINFOEX` + `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` to inherit only the
//! redirected std handles. We do exactly that here: build an explicit inherit
//! allow-list containing only three fresh `NUL` handles, so the daemon inherits
//! nothing else — regardless of what inheritable handles the client happens to
//! hold. This is the Microsoft-documented mechanism for precise handle
//! inheritance.
//!
//! Lives in this crate (not the CLI) because the CLI is
//! `#![forbid(unsafe_code)]`; the FFI here uses the same `deny(unsafe_code)`
//! opt-in as the Windows signal / parent-monitor paths.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, TRUE,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DETACHED_PROCESS,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
};

/// Spawn `exe` with `args`, fully detached (own process group, no console) and
/// inheriting only three fresh `NUL` std handles — nothing else.
///
/// `env_overrides` are applied on top of the current process environment (the
/// daemon still inherits `PATH` &c. so it can spawn PTY children).
///
/// # Errors
/// Returns the last-OS-error if any Win32 call fails.
#[allow(clippy::too_many_lines)]
pub fn spawn_detached_no_inherit(
    exe: &Path,
    args: &[OsString],
    env_overrides: &[(OsString, OsString)],
) -> io::Result<()> {
    // Three fresh NUL handles for the daemon's std streams (matches the prior
    // `Stdio::null()` behavior). std opens them non-inheritable; flip the
    // inherit bit so they can pass through the allow-list below. The `File`s
    // stay alive until the end of this fn, so the parent copies close only
    // after `CreateProcessW` has duplicated them into the child.
    let nul_in = std::fs::OpenOptions::new().read(true).open("NUL")?;
    let nul_out = std::fs::OpenOptions::new().write(true).open("NUL")?;
    let nul_err = std::fs::OpenOptions::new().write(true).open("NUL")?;
    let handles: [HANDLE; 3] = [
        nul_in.as_raw_handle() as HANDLE,
        nul_out.as_raw_handle() as HANDLE,
        nul_err.as_raw_handle() as HANDLE,
    ];
    for &h in &handles {
        // SAFETY: `h` is a valid open file handle owned by the `File`s above.
        #[allow(unsafe_code)]
        unsafe {
            SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
        }
    }

    // lpCommandLine (mutable, NUL-terminated UTF-16): "exe" arg1 arg2 …
    let mut cmdline: Vec<u16> = Vec::new();
    append_quoted(exe.as_os_str(), &mut cmdline);
    for a in args {
        cmdline.push(u16::from(b' '));
        append_quoted(a, &mut cmdline);
    }
    cmdline.push(0);

    // lpApplicationName (NUL-terminated UTF-16).
    let app: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // Environment block: current env + overrides, as a double-NUL-terminated
    // UTF-16 block (CREATE_UNICODE_ENVIRONMENT). Building it explicitly (rather
    // than passing NULL to inherit) lets us layer the override in.
    let env_block = build_env_block(env_overrides);

    // Allocate + init the attribute list holding exactly our inherit allow-list.
    let mut attr_size: usize = 0;
    // SAFETY: first call with a null list just reports the required size via
    // `attr_size` (returns FALSE / ERROR_INSUFFICIENT_BUFFER — expected).
    #[allow(unsafe_code)]
    unsafe {
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &raw mut attr_size);
    }
    if attr_size == 0 {
        // Defensive: the documented probe always reports a non-zero size; a zero
        // here would make the buffer a dangling pointer for the real init call.
        return Err(io::Error::other(
            "InitializeProcThreadAttributeList reported a zero attribute-list size",
        ));
    }
    // Over-align the backing buffer to pointer width: the attribute list stores
    // HANDLE-sized values, so a `Vec<u8>` (align 1) would only be safe by the
    // stock allocator's incidental 16-byte alignment. `Vec<usize>` guarantees it.
    let mut attr_buf: Vec<usize> = vec![0usize; attr_size.div_ceil(std::mem::size_of::<usize>())];
    let attr_list =
        attr_buf.as_mut_ptr().cast::<core::ffi::c_void>() as LPPROC_THREAD_ATTRIBUTE_LIST;
    // SAFETY: `attr_list` points at `attr_size` bytes just allocated.
    #[allow(unsafe_code)]
    let ok = unsafe { InitializeProcThreadAttributeList(attr_list, 1, 0, &raw mut attr_size) };
    if ok != TRUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handles` lives until after CreateProcessW; the attr list is init'd.
    #[allow(unsafe_code)]
    let ok = unsafe {
        UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            handles.as_ptr().cast::<core::ffi::c_void>(),
            std::mem::size_of_val(&handles),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok != TRUE {
        let e = io::Error::last_os_error();
        // SAFETY: attr_list was successfully initialized above.
        #[allow(unsafe_code)]
        unsafe {
            DeleteProcThreadAttributeList(attr_list);
        }
        return Err(e);
    }

    // STARTUPINFOEXW: use our three NUL handles as std, point at the attr list.
    // SAFETY: STARTUPINFOEXW is plain-old-data; zeroing is valid.
    #[allow(unsafe_code)]
    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>()).unwrap_or(0);
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = handles[0];
    si.StartupInfo.hStdOutput = handles[1];
    si.StartupInfo.hStdError = handles[2];
    si.lpAttributeList = attr_list;

    // SAFETY: PROCESS_INFORMATION is POD; CreateProcessW fills it in.
    #[allow(unsafe_code)]
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    let flags = DETACHED_PROCESS
        | CREATE_NEW_PROCESS_GROUP
        | EXTENDED_STARTUPINFO_PRESENT
        | CREATE_UNICODE_ENVIRONMENT;

    // SAFETY: all pointers are valid for the duration of the call; `cmdline`
    // is a mutable NUL-terminated buffer as CreateProcessW requires;
    // `bInheritHandles = TRUE` is required for the handle allow-list to take
    // effect, and the list restricts inheritance to exactly `handles`.
    #[allow(unsafe_code)]
    let created = unsafe {
        CreateProcessW(
            app.as_ptr(),
            cmdline.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            TRUE,
            flags,
            env_block.as_ptr().cast::<core::ffi::c_void>(),
            std::ptr::null(),
            std::ptr::addr_of!(si.StartupInfo),
            &raw mut pi,
        )
    };
    let create_err = if created == TRUE {
        None
    } else {
        Some(io::Error::last_os_error())
    };

    // SAFETY: attr_list was initialized; clean it up regardless of outcome.
    #[allow(unsafe_code)]
    unsafe {
        DeleteProcThreadAttributeList(attr_list);
    }
    if let Some(e) = create_err {
        return Err(e);
    }
    // We don't wait on the daemon; release the handles CreateProcessW returned.
    // SAFETY: both handles are valid non-null handles the OS just handed us.
    #[allow(unsafe_code)]
    unsafe {
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    }
    // `nul_in/out/err` drop here, closing the parent's copies; the child keeps
    // its inherited duplicates.
    Ok(())
}

/// Build a `CREATE_UNICODE_ENVIRONMENT` block: the current process environment
/// with `overrides` layered on top (case-insensitive key match, Windows env
/// semantics), as `K=V\0K=V\0…\0` UTF-16.
fn build_env_block(overrides: &[(OsString, OsString)]) -> Vec<u16> {
    use std::collections::BTreeMap;
    // Key the map on an uppercased key for Windows' case-insensitive env, but
    // keep the original key spelling for the emitted block.
    let mut map: BTreeMap<OsString, (OsString, OsString)> = BTreeMap::new();
    for (k, v) in std::env::vars_os() {
        map.insert(upper_key(&k), (k, v));
    }
    for (k, v) in overrides {
        map.insert(upper_key(k), (k.clone(), v.clone()));
    }
    let mut block: Vec<u16> = Vec::new();
    for (_, (k, v)) in map {
        // Defensive only: `vars_os()` never yields a nameless entry, but emitting
        // a "=V" line would be malformed. NOTE the Windows drive-current-dir vars
        // ("=C:", "=D:", …) are intentionally PRESERVED — they start with '=' (not
        // empty), CreateProcess both accepts and expects them, and the BTreeMap
        // sorts them first, matching the OS convention.
        if k.is_empty() {
            continue;
        }
        block.extend(k.encode_wide());
        block.push(u16::from(b'='));
        block.extend(v.encode_wide());
        block.push(0);
    }
    block.push(0); // terminating empty string
    block
}

fn upper_key(k: &OsStr) -> OsString {
    OsString::from(k.to_string_lossy().to_uppercase())
}

/// Append `arg` to `out` (UTF-16) using the standard Windows command-line
/// quoting rules (`CommandLineToArgvW`-compatible), matching what std's
/// internal `make_command_line` does.
fn append_quoted(arg: &OsStr, out: &mut Vec<u16>) {
    let wide: Vec<u16> = arg.encode_wide().collect();
    let needs_quotes = wide.is_empty()
        || wide
            .iter()
            .any(|&c| c == u16::from(b' ') || c == u16::from(b'\t') || c == u16::from(b'"'));
    if !needs_quotes {
        out.extend_from_slice(&wide);
        return;
    }
    out.push(u16::from(b'"'));
    let mut backslashes: usize = 0;
    for &c in &wide {
        if c == u16::from(b'\\') {
            backslashes += 1;
        } else if c == u16::from(b'"') {
            // Escape all pending backslashes (they precede a quote) then the quote.
            for _ in 0..=(backslashes * 2) {
                out.push(u16::from(b'\\'));
            }
            out.push(u16::from(b'"'));
            backslashes = 0;
        } else {
            for _ in 0..backslashes {
                out.push(u16::from(b'\\'));
            }
            backslashes = 0;
            out.push(c);
        }
    }
    // Escape trailing backslashes so they don't escape the closing quote.
    for _ in 0..(backslashes * 2) {
        out.push(u16::from(b'\\'));
    }
    out.push(u16::from(b'"'));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quoted(s: &str) -> String {
        let mut v = Vec::new();
        append_quoted(OsStr::new(s), &mut v);
        String::from_utf16(&v).unwrap()
    }

    #[test]
    fn quotes_only_when_needed() {
        assert_eq!(quoted("simple"), "simple");
        assert_eq!(quoted("has space"), "\"has space\"");
        assert_eq!(quoted(""), "\"\"");
    }

    #[test]
    fn escapes_embedded_quotes_and_backslashes() {
        assert_eq!(quoted("a\"b"), "\"a\\\"b\"");
        // trailing backslash inside a quoted arg is doubled
        assert_eq!(
            quoted("c:\\path with space\\"),
            "\"c:\\path with space\\\\\""
        );
    }

    #[test]
    fn env_block_is_double_null_terminated_and_nonempty() {
        let block = build_env_block(&[(OsString::from("AGENT_TUI_TEST_K"), OsString::from("V"))]);
        assert!(block.len() >= 2);
        assert_eq!(
            *block.last().unwrap(),
            0u16,
            "block ends with terminating NUL"
        );
        let s = String::from_utf16_lossy(&block);
        assert!(s.contains("AGENT_TUI_TEST_K=V"), "override present: {s:?}");
    }
}
