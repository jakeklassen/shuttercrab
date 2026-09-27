//! Window behaviour GPUI does not expose, applied to a GPUI window's `HWND`
//! (obtained through `HasWindowHandle`, PRD §17).

use anyhow::{Context, Result};
use windows::Win32::{
    Foundation::HWND,
    System::Threading::{AttachThreadInput, GetCurrentThreadId},
    UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, HWND_TOPMOST,
        SWP_NOACTIVATE, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos,
    },
};

/// Cover exactly this physical rectangle (virtual-desktop pixels) and stay
/// above other windows. GPUI positions windows in logical pixels, which can
/// leave a one-pixel gap at fractional scale factors; this pins the overlay
/// to the monitor's exact physical bounds.
pub fn cover(hwnd: isize, x: i32, y: i32, width: u32, height: u32) -> Result<()> {
    unsafe {
        SetWindowPos(
            HWND(hwnd as _),
            Some(HWND_TOPMOST),
            x,
            y,
            width as i32,
            height as i32,
            SWP_SHOWWINDOW | SWP_NOACTIVATE,
        )
    }
    .context("SetWindowPos failed")
}

/// Make the window the foreground window so it receives the keyboard
/// (Escape) and the pointer at once. A process that just received a global
/// hotkey may take the foreground; if Windows still refuses, attach to the
/// current foreground thread's input for the call.
pub fn bring_to_front(hwnd: isize) {
    let hwnd = HWND(hwnd as _);
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd {
            return;
        }
        let foreground = GetForegroundWindow();
        let theirs = GetWindowThreadProcessId(foreground, None);
        let ours = GetCurrentThreadId();
        let attached =
            theirs != 0 && theirs != ours && AttachThreadInput(ours, theirs, true).as_bool();
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if attached {
            let _ = AttachThreadInput(ours, theirs, false);
        }
    }
}
