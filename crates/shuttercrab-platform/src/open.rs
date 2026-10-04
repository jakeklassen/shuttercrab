//! Opening a file in another program: Paint, or one the user picks.

use anyhow::{Context, Result};
use std::path::Path;
use windows::{
    Win32::{
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
        UI::Shell::{OAIF_ALLOW_REGISTRATION, OAIF_EXEC, OPENASINFO, SHOpenWithDialog},
    },
    core::{HSTRING, PCWSTR},
};

/// Open `path` in Paint. Windows' Paint answers to `mspaint`, the old and
/// the new alike.
pub fn edit_in_paint(path: &Path) -> Result<()> {
    std::process::Command::new("mspaint")
        .arg(path)
        .spawn()
        .context("could not start Paint")?;
    Ok(())
}

/// Ask which program should open `path`, in Windows' own "How do you want
/// to open this file?" dialog, then open it there. The dialog waits for
/// the user, so it runs on a thread of its own.
pub fn open_with(path: &Path) {
    let path = HSTRING::from(path);
    std::thread::spawn(move || {
        // The shell's dialogs need COM on the thread that shows them.
        let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let info = OPENASINFO {
            pcszFile: PCWSTR(path.as_ptr()),
            pcszClass: PCWSTR::null(),
            oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_EXEC,
        };
        // Cancelling is an error too; there is nothing to report either way.
        if let Err(e) = unsafe { SHOpenWithDialog(None, &info) } {
            log::debug!("open with: {e}");
        }
        if com {
            unsafe { CoUninitialize() };
        }
    });
}
