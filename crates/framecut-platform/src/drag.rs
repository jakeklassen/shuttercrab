//! Dragging a file out of Framecut into other applications (PRD §7.6): the
//! Shell's own data object for the file, so Explorer, browsers and chat
//! applications receive it as if it were dragged from a folder.
//!
//! The drag image is Framecut's own. Left to itself, the Shell draws the
//! file's thumbnail from its cache, and for a file written a moment ago
//! that thumbnail is often not ready: the first drag showed a white square.

use anyhow::{Context, Result, ensure};
use std::path::Path;
use windows::{
    Win32::{
        Foundation::{POINT, SIZE},
        Graphics::Gdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDIBSection, DIB_RGB_COLORS},
        System::{
            Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IDataObject},
            Ole::{DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE},
        },
        UI::Shell::{
            BHID_DataObject, CLSID_DragDropHelper, IDragSourceHelper, IShellItem,
            SHCreateItemFromParsingName, SHDRAGIMAGE, SHDoDragDrop,
        },
    },
    core::HSTRING,
};

/// The picture under the pointer while dragging: straight-alpha RGBA8, top
/// row first.
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
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path), None)
            .with_context(|| format!("no shell item for {}", path.display()))?;
        let data: IDataObject = item
            .BindToHandler(None, &BHID_DataObject)
            .context("the file has no data object")?;
        if let Some(image) = image
            && let Err(e) = set_drag_image(&data, &image)
        {
            // The drag still works with the Shell's own picture.
            log::warn!("could not set the drag image: {e:#}");
        }
        let effect: DROPEFFECT =
            SHDoDragDrop(None, &data, None, DROPEFFECT_COPY).context("SHDoDragDrop failed")?;
        Ok(effect != DROPEFFECT_NONE)
    }
}

/// Store `image` in `data` as its drag image, held at its centre.
fn set_drag_image(data: &IDataObject, image: &DragImage) -> Result<()> {
    let (w, h) = (image.width, image.height);
    ensure!(
        w > 0 && h > 0 && image.rgba.len() == (w * h * 4) as usize,
        "drag image buffer does not match {w}x{h}"
    );
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                // Bottom-up, the orientation the drag helper reads.
                biHeight: h as i32,
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
        let row = (w * 4) as usize;
        for (y, line) in image.rgba.chunks_exact(row).enumerate() {
            let out = &mut target[(h as usize - 1 - y) * row..][..row];
            for (dst, src) in out
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(line.as_chunks::<4>().0)
            {
                // Premultiplied BGRA.
                let a = src[3] as u32;
                let pre = |c: u8| ((c as u32 * a + 127) / 255) as u8;
                *dst = [pre(src[2]), pre(src[1]), pre(src[0]), src[3]];
            }
        }
        let helper: IDragSourceHelper =
            CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER)
                .context("no drag helper")?;
        let drag = SHDRAGIMAGE {
            sizeDragImage: SIZE {
                cx: w as i32,
                cy: h as i32,
            },
            ptOffset: POINT {
                x: (w / 2) as i32,
                y: (h / 2) as i32,
            },
            hbmpDragImage: bitmap,
            // Alpha, not a colour key.
            crColorKey: windows::Win32::Foundation::COLORREF(0xFFFF_FFFF),
        };
        // On success the helper owns the bitmap.
        helper
            .InitializeFromBitmap(&drag, data)
            .context("InitializeFromBitmap failed")?;
    }
    Ok(())
}
