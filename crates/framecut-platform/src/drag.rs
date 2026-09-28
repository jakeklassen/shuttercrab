//! Dragging a file out of Framecut into other applications (PRD §7.6): the
//! Shell's own data object for the file, so Explorer, browsers and chat
//! applications receive it as if it were dragged from a folder, with the
//! standard drag image.

use anyhow::{Context, Result};
use std::path::Path;
use windows::{
    Win32::{
        System::{
            Com::IDataObject,
            Ole::{DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE},
        },
        UI::Shell::{BHID_DataObject, IShellItem, SHCreateItemFromParsingName, SHDoDragDrop},
    },
    core::HSTRING,
};

/// Drag `path` from the pointer. Call it while the left button is down, on
/// a thread with OLE initialised (GPUI's main thread), outside any GPUI
/// update: it runs a modal loop until the user drops or cancels. Returns
/// whether something accepted the file.
pub fn drag_file(path: &Path) -> Result<bool> {
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path), None)
            .with_context(|| format!("no shell item for {}", path.display()))?;
        let data: IDataObject = item
            .BindToHandler(None, &BHID_DataObject)
            .context("the file has no data object")?;
        let effect: DROPEFFECT =
            SHDoDragDrop(None, &data, None, DROPEFFECT_COPY).context("SHDoDragDrop failed")?;
        Ok(effect != DROPEFFECT_NONE)
    }
}
