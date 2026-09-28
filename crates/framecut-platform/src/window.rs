//! Window behaviour GPUI does not expose, applied to a GPUI window's `HWND`
//! (obtained through `HasWindowHandle`, PRD §17).

use anyhow::{Context, Result};
use windows::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::Gdi::ClientToScreen,
    System::Threading::{AttachThreadInput, GetCurrentThreadId},
    UI::WindowsAndMessaging::{
        BringWindowToTop, GWL_STYLE, GetClientRect, GetForegroundWindow, GetWindowLongPtrW,
        GetWindowThreadProcessId, HWND_TOPMOST, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_SHOWWINDOW,
        SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, WS_CAPTION, WS_MAXIMIZEBOX,
        WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
    },
};

/// Make the window a borderless popup covering exactly this physical
/// rectangle (virtual-desktop pixels), above other windows.
///
/// GPUI's popup windows carry `WS_CAPTION` (Windows adds it to windows
/// created with style 0), so Windows reserves invisible resize borders and
/// the drawable client area comes out about 11 px short on the left, right
/// and bottom at 150%. Replacing the frame styles with `WS_POPUP` makes the
/// client area the whole window. Positioning in physical pixels also avoids
/// the one-pixel gaps logical bounds can leave at fractional scale factors.
pub fn cover(hwnd: isize, x: i32, y: i32, width: u32, height: u32) -> Result<()> {
    let hwnd = HWND(hwnd as _);
    unsafe {
        let frame =
            (WS_CAPTION | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX).0 as isize;
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        SetWindowLongPtrW(hwnd, GWL_STYLE, (style & !frame) | WS_POPUP.0 as isize);
        // SWP_FRAMECHANGED makes Windows recompute the client area now.
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            width as i32,
            height as i32,
            SWP_SHOWWINDOW | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        )
    }
    .context("SetWindowPos failed")
}

/// The window's client area in screen coordinates: `(x, y, width, height)`.
pub fn client_bounds(hwnd: isize) -> Result<(i32, i32, u32, u32)> {
    let hwnd = HWND(hwnd as _);
    let mut rect = RECT::default();
    let mut origin = POINT::default();
    unsafe {
        GetClientRect(hwnd, &mut rect).context("GetClientRect failed")?;
        ClientToScreen(hwnd, &mut origin)
            .ok()
            .context("ClientToScreen failed")?;
    }
    Ok((origin.x, origin.y, rect.right as u32, rect.bottom as u32))
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
