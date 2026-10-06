//! Cursors GPUI does not offer on Windows, shown over part of a window.
//! GPUI draws its open and closed hands as the arrow, since Windows has no
//! such cursors, and has no four-way move arrow. A window subclass answers
//! Windows' `WM_SETCURSOR` with the chosen cursor while the pointer is over
//! the chosen part, and leaves the rest to GPUI.

use anyhow::{Context, Result};
use std::{cell::RefCell, collections::HashMap};
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{CreateBitmap, DeleteObject, ScreenToClient},
    UI::{
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            CreateIconIndirect, GetCursorPos, HCURSOR, HTCLIENT, ICONINFO, IDC_SIZEALL,
            LoadCursorW, PostMessageW, SetCursor, WM_MOUSEMOVE, WM_NCDESTROY, WM_SETCURSOR,
            WindowFromPoint,
        },
    },
};

/// A cursor Shuttercrab made, kept for as long as it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor(isize);

impl Cursor {
    /// From a `size`-pixel square of straight-alpha RGBA, pointing at
    /// `hotspot`.
    pub fn from_rgba(rgba: &[u8], size: u32, hotspot: (u32, u32)) -> Result<Cursor> {
        anyhow::ensure!(
            rgba.len() == (size * size * 4) as usize,
            "the pixels do not fill the cursor"
        );
        // Windows wants blue first, and blends a cursor's soft edges as
        // premultiplied colour: straight colour there leaves light fringes.
        let premultiplied = |c: u8, a: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
        let bgra: Vec<u8> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|&[r, g, b, a]| {
                [
                    premultiplied(b, a),
                    premultiplied(g, a),
                    premultiplied(r, a),
                    a,
                ]
            })
            .collect();
        let side = size as i32;
        unsafe {
            let color = CreateBitmap(side, side, 1, 32, Some(bgra.as_ptr().cast()));
            // Every pixel shows through the colour bitmap's alpha.
            let mask_bits = vec![0u8; (size.div_ceil(16) * 2 * size) as usize];
            let mask = CreateBitmap(side, side, 1, 1, Some(mask_bits.as_ptr().cast()));
            let info = ICONINFO {
                fIcon: false.into(),
                xHotspot: hotspot.0,
                yHotspot: hotspot.1,
                hbmMask: mask,
                hbmColor: color,
            };
            let made = CreateIconIndirect(&info);
            let _ = DeleteObject(color.into());
            let _ = DeleteObject(mask.into());
            Ok(Cursor(
                made.context("CreateIconIndirect failed")?.0 as isize,
            ))
        }
    }

    /// Windows' four-way arrow, for moving something.
    pub fn move_all() -> Result<Cursor> {
        let cursor = unsafe { LoadCursorW(None, IDC_SIZEALL) }.context("no move cursor")?;
        Ok(Cursor(cursor.0 as isize))
    }
}

/// A cursor over a part of a window: client pixels, x, y, width, height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorOver {
    pub cursor: Cursor,
    pub area: (i32, i32, i32, i32),
}

thread_local! {
    /// Each subclassed window's cursor, if it shows one now.
    static SHOWN: RefCell<HashMap<isize, Option<CursorOver>>> = RefCell::default();
}

/// The subclass's id, among any others on the window.
const SUBCLASS: usize = 0x5348_4352; // "SHCR"

/// Show `over` over part of window `hwnd`, or let GPUI choose again with
/// `None`. Call from the window's own thread; the first call subclasses
/// it. The cursor changes at once if the pointer is over the window.
pub fn show(hwnd: isize, over: Option<CursorOver>) {
    let changed = SHOWN.with(|shown| {
        let mut shown = shown.borrow_mut();
        let first = !shown.contains_key(&hwnd);
        if first {
            let installed =
                unsafe { SetWindowSubclass(HWND(hwnd as _), Some(subclass), SUBCLASS, 0) };
            if !installed.as_bool() {
                log::warn!("could not subclass the window for its cursors");
                return false;
            }
        }
        shown.insert(hwnd, over) != Some(over)
    });
    if changed && pointer_on(hwnd) {
        // As if the pointer moved: the subclass, or GPUI, sets the cursor.
        let hit = LPARAM(((WM_MOUSEMOVE as isize) << 16) | HTCLIENT as isize);
        let _ = unsafe {
            PostMessageW(
                Some(HWND(hwnd as _)),
                WM_SETCURSOR,
                WPARAM(hwnd as usize),
                hit,
            )
        };
    }
}

/// Whether the pointer is over window `hwnd`.
fn pointer_on(hwnd: isize) -> bool {
    let mut at = POINT::default();
    unsafe { GetCursorPos(&mut at).is_ok() && WindowFromPoint(at).0 as isize == hwnd }
}

/// The cursor to show at the pointer, if it is over the part shown.
fn cursor_at(hwnd: isize) -> Option<HCURSOR> {
    let over = SHOWN.with(|shown| shown.borrow().get(&hwnd).copied().flatten())?;
    let mut at = POINT::default();
    unsafe {
        GetCursorPos(&mut at).ok()?;
        ScreenToClient(HWND(hwnd as _), &mut at).ok().ok()?;
    }
    let (x, y, width, height) = over.area;
    let area = RECT {
        left: x,
        top: y,
        right: x + width,
        bottom: y + height,
    };
    let inside = (area.left..area.right).contains(&at.x) && (area.top..area.bottom).contains(&at.y);
    inside.then_some(HCURSOR(over.cursor.0 as _))
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    let window = hwnd.0 as isize;
    if message == WM_SETCURSOR
        && (lparam.0 & 0xFFFF) as u32 == HTCLIENT
        && let Some(cursor) = cursor_at(window)
    {
        unsafe { SetCursor(Some(cursor)) };
        return LRESULT(1);
    }
    if message == WM_NCDESTROY {
        SHOWN.with(|shown| shown.borrow_mut().remove(&window));
        let _ = unsafe { RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS) };
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
