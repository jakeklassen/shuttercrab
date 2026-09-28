//! Window behaviour GPUI does not expose, applied to a GPUI window's `HWND`
//! (obtained through `HasWindowHandle`, PRD §17).

use anyhow::{Context, Result};
use windows::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::Gdi::ClientToScreen,
    System::Threading::{AttachThreadInput, GetCurrentThreadId},
    UI::WindowsAndMessaging::{
        BringWindowToTop, GWL_STYLE, GetClientRect, GetForegroundWindow, GetWindowLongPtrW,
        GetWindowThreadProcessId, HWND_TOPMOST, SW_HIDE, SWP_FRAMECHANGED, SWP_NOACTIVATE,
        SWP_SHOWWINDOW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow,
        WS_CAPTION, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
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

/// Hide the window without destroying it. Hiding a GPUI window before
/// removing it lets GPUI handle the deactivation while it still knows the
/// window; otherwise the deactivation arrives during destruction and GPUI
/// logs "window not found".
pub fn hide(hwnd: isize) {
    let _ = unsafe { ShowWindow(HWND(hwnd as _), SW_HIDE) };
}

/// Leave the window out of every screen capture, Framecut's own and other
/// applications' (PRD §7.6): it is simply not there in the captured image.
pub fn exclude_from_capture(hwnd: isize) -> Result<()> {
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::SetWindowDisplayAffinity(
            HWND(hwnd as _),
            windows::Win32::UI::WindowsAndMessaging::WDA_EXCLUDEFROMCAPTURE,
        )
    }
    .context("SetWindowDisplayAffinity failed")
}

/// Give a borderless window Windows 11's rounded corners.
pub fn round_corners(hwnd: isize) {
    use windows::Win32::Graphics::Dwm::{
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
    };
    let preference = DWMWCP_ROUND;
    let _ = unsafe {
        DwmSetWindowAttribute(
            HWND(hwnd as _),
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const _ as _,
            size_of_val(&preference) as u32,
        )
    };
}

/// Keep the window from ever becoming the active window, even when
/// clicked, so the keyboard stays with the user's application.
pub fn never_activate(hwnd: isize) {
    use windows::Win32::UI::WindowsAndMessaging::{GWL_EXSTYLE, WS_EX_NOACTIVATE};
    let hwnd = HWND(hwnd as _);
    unsafe {
        let style = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_NOACTIVATE.0 as isize);
    }
}

/// The part of monitor `hmonitor` not covered by the taskbar (physical
/// pixels, virtual-desktop coordinates): x, y, width, height.
pub fn work_area(hmonitor: u64) -> Option<(i32, i32, u32, u32)> {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // Physical coordinates, whatever the calling thread's DPI awareness.
    let previous = unsafe {
        windows::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
    };
    let found = unsafe { GetMonitorInfoW(HMONITOR(hmonitor as _), &mut info) }.as_bool();
    unsafe { windows::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(previous) };
    let r = info.rcWork;
    (found && r.right > r.left && r.bottom > r.top).then(|| {
        (
            r.left,
            r.top,
            (r.right - r.left) as u32,
            (r.bottom - r.top) as u32,
        )
    })
}

/// Where the pointer is, physical virtual-desktop pixels.
pub fn cursor_position() -> Option<(i32, i32)> {
    use windows::Win32::{Foundation::POINT, UI::WindowsAndMessaging::GetCursorPos};
    let previous = unsafe {
        windows::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
    };
    let mut point = POINT::default();
    let found = unsafe { GetCursorPos(&mut point) }.is_ok();
    unsafe { windows::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(previous) };
    found.then_some((point.x, point.y))
}
