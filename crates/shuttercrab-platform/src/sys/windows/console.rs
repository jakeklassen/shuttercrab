//! Diagnostics for a windowed app started from a terminal.
//!
//! Release builds use the Windows subsystem, so no console window appears
//! when Shuttercrab starts from Explorer. But then a terminal that starts it
//! sees nothing either: the process has no console, and the console handles
//! it inherits are unusable. Attaching to the parent's console fixes that.

use std::sync::atomic::{AtomicBool, Ordering};
use windows::{
    Win32::{
        Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE},
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK,
            FILE_TYPE_PIPE, GetFileType, OPEN_EXISTING,
        },
        System::Console::{
            ATTACH_PARENT_PROCESS, AttachConsole, FreeConsole, GetStdHandle, STD_ERROR_HANDLE,
            STD_OUTPUT_HANDLE, SetStdHandle,
        },
    },
    core::w,
};

/// Which standard handles [`attach_to_parent_terminal`] pointed at the
/// terminal, for [`detach_from_terminal`] to undo.
static ATTACHED_STDOUT: AtomicBool = AtomicBool::new(false);
static ATTACHED_STDERR: AtomicBool = AtomicBool::new(false);

/// Send stdout and stderr to the terminal that started the process, if any.
/// Output already redirected to a file or pipe is left alone, and a process
/// started without a terminal is unaffected.
pub fn attach_to_parent_terminal() {
    let redirected = |handle: Result<HANDLE, _>| {
        handle
            .map(|h| {
                let kind = unsafe { GetFileType(h) };
                kind == FILE_TYPE_DISK || kind == FILE_TYPE_PIPE
            })
            .unwrap_or(false)
    };
    let stdout_redirected = redirected(unsafe { GetStdHandle(STD_OUTPUT_HANDLE) });
    let stderr_redirected = redirected(unsafe { GetStdHandle(STD_ERROR_HANDLE) });
    if (stdout_redirected && stderr_redirected)
        || unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_err()
    {
        return;
    }
    let Ok(console) = (unsafe {
        CreateFileW(
            w!("CONOUT$"),
            (GENERIC_READ | GENERIC_WRITE).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }) else {
        return;
    };
    unsafe {
        if !stdout_redirected {
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, console);
            ATTACHED_STDOUT.store(true, Ordering::Relaxed);
        }
        if !stderr_redirected {
            let _ = SetStdHandle(STD_ERROR_HANDLE, console);
            ATTACHED_STDERR.store(true, Ordering::Relaxed);
        }
    }
    // A blank line, so the first message does not share a line with the
    // prompt the shell printed when it returned.
    eprintln!();
}

/// Let go of the terminal [`attach_to_parent_terminal`] attached to, so
/// nothing more appears in it after the shell's prompt. Output redirected to
/// a file or pipe is left alone.
pub fn detach_from_terminal() {
    let out = ATTACHED_STDOUT.swap(false, Ordering::Relaxed);
    let err = ATTACHED_STDERR.swap(false, Ordering::Relaxed);
    if !out && !err {
        return;
    }
    unsafe {
        // Writes to an absent handle are dropped quietly.
        if out {
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, HANDLE::default());
        }
        if err {
            let _ = SetStdHandle(STD_ERROR_HANDLE, HANDLE::default());
        }
        let _ = FreeConsole();
    }
}
