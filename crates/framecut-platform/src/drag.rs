//! Dragging a file out of Framecut into other applications (PRD §7.6): the
//! Shell's own data object for the file, so Explorer, browsers and chat
//! applications receive it as if it were dragged from a folder.
//!
//! Framecut draws the picture under the pointer itself, in a click-through
//! layered window that follows the pointer. The Shell's drag images are
//! always drawn at about 74% opacity, and above about 300 pixels with a
//! heavy fade; Framecut's is drawn exactly as given.

use anyhow::{Context, Result, ensure};
use std::path::Path;
use windows::{
    Win32::{
        Foundation::{
            COLORREF, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HWND,
            POINT, POINTL, S_OK, SIZE,
        },
        Graphics::Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
            CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HGDIOBJ,
            SelectObject,
        },
        System::{
            Com::IDataObject,
            LibraryLoader::GetModuleHandleW,
            Ole::{
                DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, DoDragDrop, IDropSource,
                IDropSource_Impl, IDropTarget, IDropTarget_Impl, RegisterDragDrop, RevokeDragDrop,
            },
            SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS},
        },
        UI::{
            Shell::{BHID_DataObject, IShellItem, SHCreateItemFromParsingName},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, HWND_TOPMOST,
                RegisterClassW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE, SetWindowPos,
                ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WNDCLASSW, WS_EX_LAYERED,
                WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
            },
        },
    },
    core::{BOOL, HRESULT, HSTRING, implement, w},
};

/// The picture under the pointer while dragging: straight-alpha RGBA8, top
/// row first, drawn as given (any softness or transparency baked in),
/// centred on the pointer.
pub struct DragImage<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
}

/// Drag `path` from the pointer, showing `image` if given. Call it while
/// the left button is down, on a thread with OLE initialised (GPUI's main
/// thread), outside any GPUI update: it runs a modal loop until the user
/// drops or cancels. Returns whether something accepted the file.
pub fn drag_file(path: &Path, image: Option<DragImage>) -> Result<bool> {
    let data: IDataObject = unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path), None)
            .with_context(|| format!("no shell item for {}", path.display()))?;
        item.BindToHandler(None, &BHID_DataObject)
            .context("the file has no data object")?
    };
    let ghost = image.and_then(|image| match Ghost::new(&image) {
        Ok(ghost) => Some(ghost),
        Err(e) => {
            // The drag still works, without a picture.
            log::warn!("could not draw the drag image: {e:#}");
            None
        }
    });
    if let Some(ghost) = &ghost {
        ghost.follow();
        ghost.show();
    }
    let source: IDropSource = DropSource {
        ghost: ghost.as_ref().map(|g| (g.hwnd, g.half)),
    }
    .into();
    let mut effect = DROPEFFECT_NONE;
    let result = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
    drop(ghost);
    match result {
        r if r == DRAGDROP_S_DROP => Ok(effect != DROPEFFECT_NONE),
        r if r == DRAGDROP_S_CANCEL => Ok(false),
        r => Err(windows::core::Error::from_hresult(r)).context("DoDragDrop failed"),
    }
}

/// Keeps the picture under the pointer and decides when the drag ends.
#[implement(IDropSource)]
struct DropSource {
    /// The picture's window and half its size.
    ghost: Option<(isize, (i32, i32))>,
}

impl DropSource {
    fn follow(&self) {
        if let Some((hwnd, half)) = self.ghost {
            move_to_pointer(HWND(hwnd as _), half);
        }
    }
}

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        self.follow();
        if escape.as_bool() {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
        self.follow();
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

fn move_to_pointer(hwnd: HWND, half: (i32, i32)) {
    let mut at = POINT::default();
    unsafe {
        if GetCursorPos(&mut at).is_ok() {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                at.x - half.0,
                at.y - half.1,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}

extern "system" fn ghost_proc(
    hwnd: HWND,
    message: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// A click-through, never-activated layered window showing the picture.
struct Ghost {
    hwnd: isize,
    half: (i32, i32),
}

impl Ghost {
    fn new(image: &DragImage) -> Result<Self> {
        let (w, h) = (image.width, image.height);
        ensure!(
            w > 0 && h > 0 && image.rgba.len() == (w * h * 4) as usize,
            "drag image buffer does not match {w}x{h}"
        );
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(ghost_proc),
                hInstance: instance.into(),
                lpszClassName: w!("FramecutDragImage"),
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
                w!("FramecutDragImage"),
                None,
                WS_POPUP,
                0,
                0,
                w as i32,
                h as i32,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .context("could not create the drag image window")?;
            let ghost = Ghost {
                hwnd: hwnd.0 as isize,
                half: ((w / 2) as i32, (h / 2) as i32),
            };
            // Never part of a screenshot (FRAMECUT_CAPTURABLE_UI keeps it
            // capturable, for screenshots of Framecut itself).
            if std::env::var_os("FRAMECUT_CAPTURABLE_UI").is_none() {
                let _ = crate::window::exclude_from_capture(ghost.hwnd);
            }
            ghost.paint(image)?;
            Ok(ghost)
        }
    }

    /// Put the premultiplied picture into the layered window.
    fn paint(&self, image: &DragImage) -> Result<()> {
        let (w, h) = (image.width, image.height);
        unsafe {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32),
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
            let target = std::slice::from_raw_parts_mut(bits.cast::<u8>(), image.rgba.len());
            for (dst, src) in target
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(image.rgba.as_chunks::<4>().0)
            {
                let a = src[3] as u32;
                let pre = |c: u8| ((c as u32 * a + 127) / 255) as u8;
                *dst = [pre(src[2]), pre(src[1]), pre(src[0]), src[3]];
            }
            let dc = CreateCompatibleDC(None);
            let previous = SelectObject(dc, HGDIOBJ(bitmap.0));
            let size = SIZE {
                cx: w as i32,
                cy: h as i32,
            };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let result = UpdateLayeredWindow(
                HWND(self.hwnd as _),
                None,
                None,
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

    fn follow(&self) {
        move_to_pointer(HWND(self.hwnd as _), self.half);
    }

    fn show(&self) {
        unsafe {
            let _ = ShowWindow(HWND(self.hwnd as _), SW_SHOWNOACTIVATE);
        }
    }
}

impl Drop for Ghost {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(HWND(self.hwnd as _)) };
    }
}

/// Make the window refuse drops. GPUI registers every window as a drop
/// target; a window that is itself the source of drags should not be one.
/// GPUI's target is swapped for one that always answers "no drop", so
/// GPUI can still revoke a registered target when the window closes.
pub fn refuse_drops(hwnd: isize) -> Result<()> {
    let hwnd = HWND(hwnd as _);
    unsafe {
        let _ = RevokeDragDrop(hwnd);
        let target: IDropTarget = NoDrop.into();
        RegisterDragDrop(hwnd, &target).context("RegisterDragDrop failed")
    }
}

#[implement(IDropTarget)]
struct NoDrop;

impl IDropTarget_Impl for NoDrop_Impl {
    fn DragEnter(
        &self,
        _: windows::core::Ref<IDataObject>,
        _: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        refuse(effect);
        Ok(())
    }

    fn DragOver(
        &self,
        _: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        refuse(effect);
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn Drop(
        &self,
        _: windows::core::Ref<IDataObject>,
        _: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        refuse(effect);
        Ok(())
    }
}

fn refuse(effect: *mut DROPEFFECT) {
    if !effect.is_null() {
        // SAFETY: OLE passes a valid pointer for the effect.
        unsafe { *effect = DROPEFFECT_NONE };
    }
}
