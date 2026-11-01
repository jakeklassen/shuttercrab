//! Cursors GPUI does not offer on Windows, shown over part of a window.
//! GPUI draws its open and closed hands as the arrow, since Windows has no
//! such cursors, and has no four-way move arrow. A window subclass answers
//! Windows' `WM_SETCURSOR` with the chosen cursor while the pointer is over
//! the chosen part, and leaves the rest to GPUI.

use crate::cursor::CursorOver;
use anyhow::{Context, Result};
use shuttercrab_types::WindowId;
use std::{cell::RefCell, collections::HashMap};
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
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
            (size as usize)
                .checked_mul(size as usize)
                .and_then(|n| n.checked_mul(4))
                == Some(rgba.len()),
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
        // SAFETY: CreateBitmap reads `side` rows of `side` 32-bit pixels
        // from `bgra`, which holds exactly that (checked above without
        // overflow, which also keeps `side` from going negative), and
        // `side` rows of `side` bits, each padded to a 2-byte word, from
        // `mask_bits`, which is that long. With `side` 0 it reads nothing
        // and gives a stock 1×1 bitmap. Both buffers live through the
        // calls. The bitmaps are deleted once the cursor has copied them.
        unsafe {
            let color = CreateBitmap(side, side, 1, 32, Some(bgra.as_ptr().cast()));
            // Every pixel shows through the colour bitmap's alpha.
            let mask_bits = vec![0u8; size.div_ceil(16) as usize * 2 * size as usize];
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
        // SAFETY: loads a system cursor by its id; with no module there is
        // nothing else to pass. Shared system cursors are never destroyed.
        let cursor = unsafe { LoadCursorW(None, IDC_SIZEALL) }.context("no move cursor")?;
        Ok(Cursor(cursor.0 as isize))
    }
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
pub fn show(window: WindowId, over: Option<CursorOver>) {
    let hwnd = window.raw() as isize;
    let changed = SHOWN.with(|shown| {
        let mut shown = shown.borrow_mut();
        let first = !shown.contains_key(&hwnd);
        if first {
            // SAFETY: `subclass` is a static function that fits the
            // subclass signature and uses no reference data. Windows checks
            // `hwnd`, and the call fails for a window of another thread.
            // The subclass removes itself when the window is destroyed.
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
        // SAFETY: posts plain values, no pointers; Windows checks `hwnd`.
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
    // SAFETY: `at` is a live POINT for GetCursorPos to write; WindowFromPoint
    // takes it by value.
    unsafe { GetCursorPos(&mut at).is_ok() && WindowFromPoint(at).0 as isize == hwnd }
}

/// The cursor to show at the pointer, if it is over the part shown.
fn cursor_at(hwnd: isize) -> Option<HCURSOR> {
    let over = SHOWN.with(|shown| shown.borrow().get(&hwnd).copied().flatten())?;
    let mut at = POINT::default();
    // SAFETY: `at` is a live POINT for both calls to write. Windows checks
    // `hwnd`; a window gone since makes ScreenToClient fail.
    unsafe {
        GetCursorPos(&mut at).ok()?;
        ScreenToClient(HWND(hwnd as _), &mut at).ok().ok()?;
    }
    over.area
        .contains(at.x, at.y)
        .then_some(HCURSOR(over.cursor.0 as _))
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
        // SAFETY: `cursor` is a `Cursor`'s handle, which is never destroyed
        // while Shuttercrab runs, and this is the window's own thread.
        unsafe { SetCursor(Some(cursor)) };
        return LRESULT(1);
    }
    if message == WM_NCDESTROY {
        SHOWN.with(|shown| shown.borrow_mut().remove(&window));
        // SAFETY: called on the window's thread, from the subclass being
        // removed, as Windows' last message to it.
        let _ = unsafe { RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS) };
    }
    // SAFETY: passes on, unchanged, the message Windows gave this subclass,
    // on the same thread.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
