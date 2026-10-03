//! Shuttercrab's taskbar button: a window that is always minimized, so it
//! has a button but nothing to show. Clicking the button (or choosing the
//! window in Alt+Tab) asks to restore it; the restore is refused and
//! reported as [`PlatformEvent::TaskbarClicked`]. Closing it from the
//! button's menu is reported as [`PlatformEvent::TaskbarClosed`].
//!
//! It lives on the platform thread, like the tray icon.

use crate::{PlatformEvent, icon, with_state};
use anyhow::{Context, Result};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, GetSystemMetrics, HICON,
            ICON_BIG, ICON_SMALL, IsWindowVisible, RegisterClassW, SC_CLOSE, SC_MAXIMIZE,
            SC_RESTORE, SIZE_MINIMIZED, SM_CXICON, SM_CXSMICON, SW_SHOWMINNOACTIVE, SendMessageW,
            ShowWindow, WM_CLOSE, WM_SETICON, WM_SIZE, WM_SYSCOMMAND, WNDCLASSW, WS_CAPTION,
            WS_EX_APPWINDOW, WS_MINIMIZEBOX, WS_SYSMENU,
        },
    },
    core::w,
};

/// The button's window and the icons it shows.
pub(crate) struct TaskbarButton {
    window: HWND,
    icons: Vec<HICON>,
}

impl TaskbarButton {
    /// Add the button. Call on the platform thread.
    pub(crate) fn show() -> Result<Self> {
        let window = unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: w!("ShuttercrabTaskbar"),
                ..Default::default()
            };
            // A second registration fails harmlessly.
            RegisterClassW(&class);
            CreateWindowExW(
                WS_EX_APPWINDOW,
                w!("ShuttercrabTaskbar"),
                w!("Shuttercrab"),
                WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .context("could not create the taskbar window")?
        };
        let mut icons = Vec::new();
        for (kind, metric) in [(ICON_BIG, SM_CXICON), (ICON_SMALL, SM_CXSMICON)] {
            let size = unsafe { GetSystemMetrics(metric) }.max(16) as u32;
            match icon::hicon(size) {
                Ok(icon) => {
                    unsafe {
                        SendMessageW(
                            window,
                            WM_SETICON,
                            Some(WPARAM(kind as usize)),
                            Some(LPARAM(icon.0 as isize)),
                        )
                    };
                    icons.push(icon);
                }
                Err(e) => log::warn!("could not draw the taskbar icon: {e:#}"),
            }
        }
        // Minimized from the start, without taking the keyboard.
        unsafe {
            let _ = ShowWindow(window, SW_SHOWMINNOACTIVE);
        }
        log::debug!("taskbar button added");
        Ok(Self { window, icons })
    }
}

impl Drop for TaskbarButton {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.window);
            for icon in self.icons.drain(..) {
                let _ = DestroyIcon(icon);
            }
        }
        log::debug!("taskbar button removed");
    }
}

fn report(event: PlatformEvent) {
    with_state(|s| s.events.unbounded_send(event));
}

extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_SYSCOMMAND => match (wparam.0 & 0xFFF0) as u32 {
            // Stay minimized: there is no window to show, only the capture.
            SC_RESTORE | SC_MAXIMIZE => {
                report(PlatformEvent::TaskbarClicked);
                return LRESULT(0);
            }
            SC_CLOSE => {
                report(PlatformEvent::TaskbarClosed);
                return LRESULT(0);
            }
            _ => {}
        },
        WM_CLOSE => {
            report(PlatformEvent::TaskbarClosed);
            return LRESULT(0);
        }
        // Restored some other way: back down, and treat it as a click. (Not
        // the sizing during creation, before it is shown.)
        WM_SIZE
            if wparam.0 as u32 != SIZE_MINIMIZED && unsafe { IsWindowVisible(hwnd) }.as_bool() =>
        {
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
            }
            report(PlatformEvent::TaskbarClicked);
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}
