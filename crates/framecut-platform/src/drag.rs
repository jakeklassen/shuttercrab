//! Dragging a file out of Framecut into other applications (PRD §7.6): the
//! Shell's own data object for the file, so Explorer, browsers and chat
//! applications receive it as if it were dragged from a folder.
//!
//! Framecut draws the picture under the pointer itself, in a click-through
//! layered window that follows the pointer. The Shell's drag images are
//! always drawn at about 74% opacity, and above about 300 pixels with a
//! heavy fade; Framecut's is drawn exactly as given.

use crate::layered::LayeredWindow;
use anyhow::{Context, Result, ensure};
use std::path::Path;
use windows::{
    Win32::{
        Foundation::{
            DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HWND, POINT, POINTL,
            S_OK,
        },
        System::{
            Com::IDataObject,
            Ole::{
                DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, DoDragDrop, IDropSource,
                IDropSource_Impl, IDropTarget, IDropTarget_Impl, RegisterDragDrop, RevokeDragDrop,
            },
            SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS},
        },
        UI::{
            Shell::{BHID_DataObject, IShellItem, SHCreateItemFromParsingName},
            WindowsAndMessaging::{
                GetCursorPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOSIZE, SetWindowPos,
            },
        },
    },
    core::{BOOL, HRESULT, HSTRING, implement},
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
        ghost: ghost.as_ref().map(|g| (g.hwnd(), g.half)),
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

/// The picture under the pointer: a click-through layered window.
struct Ghost {
    window: LayeredWindow,
    half: (i32, i32),
}

impl Ghost {
    fn new(image: &DragImage) -> Result<Self> {
        let (w, h) = (image.width, image.height);
        ensure!(
            w > 0 && h > 0 && image.rgba.len() == (w * h * 4) as usize,
            "drag image buffer does not match {w}x{h}"
        );
        let window = LayeredWindow::new().context("could not create the drag image window")?;
        // Never part of a screenshot (FRAMECUT_CAPTURABLE_UI keeps it
        // capturable, for screenshots of Framecut itself).
        if std::env::var_os("FRAMECUT_CAPTURABLE_UI").is_none() {
            let _ = crate::window::exclude_from_capture(window.hwnd);
        }
        // Premultiplied BGRA, as layered windows take it.
        let bgra: Vec<u8> = image
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|src| {
                let a = src[3] as u32;
                let pre = |c: u8| ((c as u32 * a + 127) / 255) as u8;
                [pre(src[2]), pre(src[1]), pre(src[0]), src[3]]
            })
            .collect();
        window.paint(Some((0, 0)), w, h, &bgra)?;
        Ok(Ghost {
            window,
            half: ((w / 2) as i32, (h / 2) as i32),
        })
    }

    fn hwnd(&self) -> isize {
        self.window.hwnd
    }

    fn follow(&self) {
        move_to_pointer(HWND(self.hwnd() as _), self.half);
    }

    fn show(&self) {
        self.window.show();
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
