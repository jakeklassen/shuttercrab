//! Finding the windows a screenshot can target on Windows.

use crate::targets::{Bounds, WindowTarget};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, RECT},
        Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
        System::Threading::GetCurrentProcessId,
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
            WindowsAndMessaging::{
                EnumWindows, GWL_EXSTYLE, GetClassNameW, GetWindowDisplayAffinity,
                GetWindowLongPtrW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
                WDA_EXCLUDEFROMCAPTURE, WS_EX_TRANSPARENT,
            },
        },
    },
    core::BOOL,
};
/// virtual desktops, suspended apps) and click-through windows (overlays
/// that draw over everything) are left out, and so is Shuttercrab's own
/// capture UI (excluded from capture).
pub fn visible_windows() -> Vec<WindowTarget> {
    let mut found: Vec<WindowTarget> = Vec::new();
    // Physical coordinates, whatever the calling thread's DPI awareness.
    let previous =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    unsafe {
        let _ = EnumWindows(
            Some(visit),
            LPARAM(&mut found as *mut Vec<WindowTarget> as isize),
        );
        SetThreadDpiAwarenessContext(previous);
    }
    found
}

extern "system" fn visit(hwnd: HWND, found: LPARAM) -> BOOL {
    // SAFETY: `visible_windows` passes a live `&mut Vec` for the duration of
    // EnumWindows, which calls back synchronously on the same thread.
    let found = unsafe { &mut *(found.0 as *mut Vec<WindowTarget>) };
    if let Some(target) = describe(hwnd) {
        found.push(target);
    }
    true.into()
}

fn describe(hwnd: HWND) -> Option<WindowTarget> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        // Shuttercrab's own capture UI (thumbnail, Capture Bar, drag image) is
        // excluded from capture and never a target; its ordinary windows,
        // such as Settings, are targets like any other application's.
        let mut process = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut process));
        if process == GetCurrentProcessId() {
            let mut affinity = 0u32;
            if GetWindowDisplayAffinity(hwnd, &mut affinity).is_ok()
                && affinity == WDA_EXCLUDEFROMCAPTURE.0
            {
                return None;
            }
        }
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex_style & WS_EX_TRANSPARENT.0 != 0 {
            return None;
        }
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as _,
            size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
        {
            return None;
        }
        let mut rect = RECT::default();
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as _,
            size_of::<RECT>() as u32,
        )
        .ok()?;
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        if width < 2 || height < 2 {
            return None;
        }
        let mut name = [0u16; 128];
        let length = GetClassNameW(hwnd, &mut name);
        let class = String::from_utf16_lossy(&name[..length.max(0) as usize]);
        Some(WindowTarget {
            hwnd: hwnd.0 as isize,
            bounds: Bounds {
                x: rect.left,
                y: rect.top,
                width: width as u32,
                height: height as u32,
            },
            desktop: matches!(class.as_str(), "Progman" | "WorkerW"),
            class,
        })
    }
}
