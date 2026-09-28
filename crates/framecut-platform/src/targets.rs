//! The windows a screenshot can target: visible top-level windows, front to
//! back, with the bounds the user sees (PRD §7.3).

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, RECT},
        Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
        System::Threading::GetCurrentProcessId,
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
            WindowsAndMessaging::{
                EnumWindows, GWL_EXSTYLE, GetClassNameW, GetWindowLongPtrW,
                GetWindowThreadProcessId, IsIconic, IsWindowVisible, WS_EX_TRANSPARENT,
            },
        },
    },
    core::BOOL,
};

/// A rectangle in physical virtual-desktop pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Bounds {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x
            && y >= self.y
            && (x - self.x) < self.width as i32
            && (y - self.y) < self.height as i32
    }

    pub fn right(&self) -> i32 {
        self.x + self.width as i32
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.height as i32
    }

    /// The part inside `other`, if any.
    pub fn intersect(&self, other: &Bounds) -> Option<Bounds> {
        let (x0, y0) = (self.x.max(other.x), self.y.max(other.y));
        let (x1, y1) = (
            self.right().min(other.right()),
            self.bottom().min(other.bottom()),
        );
        (x1 > x0 && y1 > y0).then(|| Bounds {
            x: x0,
            y: y0,
            width: (x1 - x0) as u32,
            height: (y1 - y0) as u32,
        })
    }
}

/// A window on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowTarget {
    pub hwnd: isize,
    /// What the user sees of the window, without the invisible resize
    /// borders: the area Windows.Graphics.Capture captures.
    pub bounds: Bounds,
    /// The desktop (wallpaper and icons) rather than an application window.
    pub desktop: bool,
    /// The window class, for diagnostics. Never logged with titles.
    pub class: String,
}

/// Visible top-level windows of other processes, front to back. Minimised,
/// cloaked (other virtual desktops, suspended apps) and click-through
/// windows (overlays that draw over everything) are left out.
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
        let mut process = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut process));
        if process == GetCurrentProcessId() {
            return None;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersects_and_contains() {
        let a = Bounds {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        let b = Bounds {
            x: 5,
            y: -5,
            width: 10,
            height: 10,
        };
        assert_eq!(
            a.intersect(&b),
            Some(Bounds {
                x: 5,
                y: 0,
                width: 5,
                height: 5
            })
        );
        assert_eq!(
            a.intersect(&Bounds {
                x: 10,
                y: 0,
                width: 5,
                height: 5
            }),
            None
        );
        assert!(a.contains(0, 0) && a.contains(9, 9));
        assert!(!a.contains(10, 5) && !a.contains(-1, 5));
    }
}
