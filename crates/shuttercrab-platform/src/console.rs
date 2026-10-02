//! Diagnostics for a windowed app started from a terminal.
//!
//! Release builds use the Windows subsystem, so no console window appears
//! when Shuttercrab starts from Explorer. But then a terminal that starts it
//! sees nothing either: the process has no console, and the console handles
//! it inherits are unusable. Attaching to the parent's console fixes that.

use windows::{
    Win32::{
        Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE},
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK,
            FILE_TYPE_PIPE, GetFileType, OPEN_EXISTING,
        },
        System::Console::{
            ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE,
            STD_OUTPUT_HANDLE, SetStdHandle,
        },
    },
    core::w,
};

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
        }
        if !stderr_redirected {
            let _ = SetStdHandle(STD_ERROR_HANDLE, console);
        }
    }
    // A blank line, so the first message does not share a line with the
    // prompt the shell printed when it returned.
    eprintln!();
}
