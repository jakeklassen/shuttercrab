//! Click-through, never-activated windows that show a picture with
//! per-pixel alpha (`UpdateLayeredWindow`): the drag image and the border
//! around a recorded area. Fully transparent pixels show what is beneath,
//! and the pointer always passes through to it.

use anyhow::{Context, Result, ensure};
use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM},
        Graphics::Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
            CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HGDIOBJ,
            SelectObject,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SW_SHOWNOACTIVATE,
            ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
            WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
        },
    },
    core::w,
};

extern "system" fn layered_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// A topmost layered window, hidden until [`LayeredWindow::show`]. Create
/// and drop it on a thread that dispatches window messages (GPUI's main
/// thread does).
pub(crate) struct LayeredWindow {
    pub(crate) hwnd: isize,
}

impl LayeredWindow {
    pub(crate) fn new() -> Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(layered_proc),
                hInstance: instance.into(),
                lpszClassName: w!("ShuttercrabLayered"),
                ..Default::default()
            };
            // Registering again fails harmlessly.
            RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE,
                w!("ShuttercrabLayered"),
                None,
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .context("could not create a layered window")?;
            Ok(Self {
                hwnd: hwnd.0 as isize,
            })
        }
    }

    /// Show `bgra` (premultiplied BGRA8, top row first, `width`×`height`)
    /// with its top-left corner at `at`, physical pixels, or where the
    /// window already is if `at` is `None`.
    pub(crate) fn paint(
        &self,
        at: Option<(i32, i32)>,
        width: u32,
        height: u32,
        bgra: &[u8],
    ) -> Result<()> {
        ensure!(
            width > 0 && height > 0 && bgra.len() == (width * height * 4) as usize,
            "layered picture does not match {width}x{height}"
        );
        unsafe {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)
                .context("CreateDIBSection failed")?;
            std::slice::from_raw_parts_mut(bits.cast::<u8>(), bgra.len()).copy_from_slice(bgra);
            let dc = CreateCompatibleDC(None);
            let previous = SelectObject(dc, HGDIOBJ(bitmap.0));
            let size = SIZE {
                cx: width as i32,
                cy: height as i32,
            };
            let position = at.map(|(x, y)| POINT { x, y });
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let result = UpdateLayeredWindow(
                HWND(self.hwnd as _),
                None,
                position.as_ref().map(|p| p as *const POINT),
                Some(&size),
                Some(dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            SelectObject(dc, previous);
            let _ = DeleteDC(dc);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            result.context("UpdateLayeredWindow failed")
        }
    }

    pub(crate) fn show(&self) {
        unsafe {
            let _ = ShowWindow(HWND(self.hwnd as _), SW_SHOWNOACTIVATE);
        }
    }
}

impl Drop for LayeredWindow {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(HWND(self.hwnd as _)) };
    }
}
