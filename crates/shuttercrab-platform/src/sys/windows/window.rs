//! Window behaviour GPUI does not expose, applied to a window by its
//! `HWND` (PRD §17).

use anyhow::{Context, Result};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use shuttercrab_types::{MonitorId, WindowId};
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

/// The window's outer bounds, physical virtual-desktop pixels:
/// `(x, y, width, height)`, as [`place`] takes them.
pub fn outer_bounds(hwnd: WindowId) -> Result<(i32, i32, u32, u32)> {
    let mut rect = RECT::default();
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(to_hwnd(hwnd), &mut rect) }
        .context("GetWindowRect failed")?;
    Ok((
        rect.left,
        rect.top,
        (rect.right - rect.left) as u32,
        (rect.bottom - rect.top) as u32,
    ))
}

/// Put the window at `bounds` (as [`outer_bounds`] gave them), without
/// taking the keyboard or changing which windows are in front.
pub fn place(hwnd: WindowId, (x, y, width, height): (i32, i32, u32, u32)) -> Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER;
    unsafe {
        SetWindowPos(
            to_hwnd(hwnd),
            None,
            x,
            y,
            width as i32,
            height as i32,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .context("SetWindowPos failed")
}

/// The process whose sound is `window`'s: the window's own, or for a
/// packaged app, whose frame belongs to ApplicationFrameHost, the app's
/// window inside the frame.
pub fn sound_process(window: WindowId) -> Option<u32> {
    use windows::{
        Win32::UI::WindowsAndMessaging::{FindWindowExW, GetClassNameW},
        core::{PCWSTR, w},
    };
    let hwnd = to_hwnd(window);
    let process_of = |hwnd: HWND| {
        let mut process = 0;
        (unsafe { GetWindowThreadProcessId(hwnd, Some(&mut process)) } != 0 && process != 0)
            .then_some(process)
    };
    let mut name = [0u16; 64];
    let length = unsafe { GetClassNameW(hwnd, &mut name) };
    let class = String::from_utf16_lossy(&name[..length.max(0) as usize]);
    if class == "ApplicationFrameWindow"
        && let Ok(app) = unsafe {
            FindWindowExW(
                Some(hwnd),
                None,
                w!("Windows.UI.Core.CoreWindow"),
                PCWSTR::null(),
            )
        }
    {
        return process_of(app);
    }
    process_of(hwnd)
}

/// The monitor (its `HMONITOR`) most of `hwnd` is on.
pub fn monitor_of(hwnd: WindowId) -> MonitorId {
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
    MonitorId::from_raw(
        unsafe { MonitorFromWindow(to_hwnd(hwnd), MONITOR_DEFAULTTONEAREST) }.0 as u64,
    )
}

/// Make the window a borderless popup covering exactly this physical
/// rectangle (virtual-desktop pixels), above other windows.
///
/// GPUI's popup windows carry `WS_CAPTION` (Windows adds it to windows
/// created with style 0), so Windows reserves invisible resize borders and
/// the drawable client area comes out about 11 px short on the left, right
/// and bottom at 150%. Replacing the frame styles with `WS_POPUP` makes the
/// client area the whole window. Positioning in physical pixels also avoids
/// the one-pixel gaps logical bounds can leave at fractional scale factors.
pub fn cover(hwnd: WindowId, x: i32, y: i32, width: u32, height: u32) -> Result<()> {
    let hwnd = to_hwnd(hwnd);
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
pub fn client_bounds(hwnd: WindowId) -> Result<(i32, i32, u32, u32)> {
    let hwnd = to_hwnd(hwnd);
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
pub fn bring_to_front(hwnd: WindowId) {
    let hwnd = to_hwnd(hwnd);
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

/// The window that has the keyboard, to give it back later with
/// [`bring_to_front`].
pub fn foreground_window() -> Option<WindowId> {
    let hwnd = unsafe { GetForegroundWindow() };
    (!hwnd.is_invalid()).then_some(to_window_id(hwnd))
}

/// Hide the window without destroying it. Hiding a GPUI window before
/// removing it lets GPUI handle the deactivation while it still knows the
/// window; otherwise the deactivation arrives during destruction and GPUI
/// logs "window not found".
pub fn hide(hwnd: WindowId) {
    let _ = unsafe { ShowWindow(to_hwnd(hwnd), SW_HIDE) };
}

/// Leave the window out of every screen capture, Shuttercrab's own and other
/// applications' (PRD §7.6): it is simply not there in the captured image.
pub fn exclude_from_capture(hwnd: WindowId) -> Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowDisplayAffinity, SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
    };
    unsafe { SetWindowDisplayAffinity(to_hwnd(hwnd), WDA_EXCLUDEFROMCAPTURE) }
        .context("SetWindowDisplayAffinity failed")?;
    // PRD §13.6: check what Windows applied. Before Windows 10 2004 the
    // call succeeds with WDA_MONITOR instead, which shows a black box.
    let mut applied = 0;
    unsafe { GetWindowDisplayAffinity(to_hwnd(hwnd), &mut applied) }
        .context("GetWindowDisplayAffinity failed")?;
    anyhow::ensure!(
        applied == WDA_EXCLUDEFROMCAPTURE.0,
        "display affinity is {applied:#x}, not WDA_EXCLUDEFROMCAPTURE"
    );
    Ok(())
}

/// Let screen captures see the window again, after
/// [`exclude_from_capture`].
pub fn include_in_capture(hwnd: WindowId) {
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_NONE};
    let _ = unsafe { SetWindowDisplayAffinity(to_hwnd(hwnd), WDA_NONE) };
}

/// Whether the window is on screen: shown and not minimised.
pub fn is_on_screen(hwnd: WindowId) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindowVisible};
    let hwnd = to_hwnd(hwnd);
    unsafe { IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() }
}

/// Give a borderless window Windows 11's rounded corners.
pub fn round_corners(hwnd: WindowId) {
    use windows::Win32::Graphics::Dwm::{
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
    };
    let preference = DWMWCP_ROUND;
    let _ = unsafe {
        DwmSetWindowAttribute(
            to_hwnd(hwnd),
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const _ as _,
            size_of_val(&preference) as u32,
        )
    };
}

/// Keep the window from ever becoming the active window, even when
/// clicked, so the keyboard stays with the user's application.
pub fn never_activate(hwnd: WindowId) {
    use windows::Win32::UI::WindowsAndMessaging::{GWL_EXSTYLE, WS_EX_NOACTIVATE};
    let hwnd = to_hwnd(hwnd);
    unsafe {
        let style = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_NOACTIVATE.0 as isize);
    }
}

/// The part of `monitor` not covered by the taskbar (physical
/// pixels, virtual-desktop coordinates): x, y, width, height.
pub fn work_area(monitor: MonitorId) -> Option<(i32, i32, u32, u32)> {
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
    let found = unsafe { GetMonitorInfoW(HMONITOR(monitor.raw() as _), &mut info) }.as_bool();
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

/// Resize a normal window so its client area is `width` × `height`
/// physical pixels, with the whole window at most `share` of its monitor's
/// work area across and down, but its client area at least `least`
/// (width, height) where the work area allows. It keeps its centre, moved
/// only as far as needed to stay on that monitor. Works on a hidden window
/// too.
pub fn fit_client_area(
    hwnd: WindowId,
    (width, height): (u32, u32),
    share: f32,
    least: (u32, u32),
) -> Result<()> {
    use windows::Win32::{
        Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
        },
        UI::WindowsAndMessaging::{GetWindowRect, SWP_NOZORDER},
    };
    let hwnd = to_hwnd(hwnd);
    let (mut window, mut client) = (RECT::default(), RECT::default());
    let mut monitor = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        GetWindowRect(hwnd, &mut window).context("GetWindowRect failed")?;
        GetClientRect(hwnd, &mut client).context("GetClientRect failed")?;
        GetMonitorInfoW(
            MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
            &mut monitor,
        )
        .ok()
        .context("GetMonitorInfoW failed")?;
    }
    // The title bar and borders around the client area.
    let frame_width = (window.right - window.left) - client.right;
    let frame_height = (window.bottom - window.top) - client.bottom;
    let work = monitor.rcWork;
    // The share of the work area, but not below the least size, and never
    // beyond the work area itself.
    let most = |side: i32, least: i32| {
        ((side as f32 * share.clamp(0., 1.)) as i32)
            .max(least)
            .min(side)
    };
    let (work_width, work_height) = (work.right - work.left, work.bottom - work.top);
    let w = (width as i32 + frame_width).min(most(work_width, least.0 as i32 + frame_width));
    let h = (height as i32 + frame_height).min(most(work_height, least.1 as i32 + frame_height));
    let x = ((window.left + window.right) / 2 - w / 2).clamp(work.left, work.right - w);
    let y = ((window.top + window.bottom) / 2 - h / 2).clamp(work.top, work.bottom - h);
    unsafe { SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE) }
        .context("SetWindowPos failed")
}

/// Show the window normally and bring it to the front. A launcher's "start
/// hidden" or "start minimised" applies to a process's first window shown
/// the usual way; this overrides it.
pub fn show_normal(hwnd: WindowId) {
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let _ = unsafe { ShowWindow(to_hwnd(hwnd), SW_SHOWNORMAL) };
    bring_to_front(hwnd);
}

/// The `HWND` a [`WindowId`] names.
pub(crate) fn to_hwnd(id: WindowId) -> HWND {
    HWND(id.raw() as _)
}

/// The [`WindowId`] for an `HWND`.
pub(crate) fn to_window_id(hwnd: HWND) -> WindowId {
    WindowId::from_raw(hwnd.0 as u64)
}

/// The window behind a GPUI window (or anything else with a native
/// handle), if it is a Win32 one.
pub fn of(window: &impl HasWindowHandle) -> Option<WindowId> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(win32) => Some(WindowId::from_raw(win32.hwnd.get() as u64)),
        _ => None,
    }
}
