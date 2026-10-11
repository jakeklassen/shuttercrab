//! Putting a screenshot or files on the clipboard under X11, through
//! arboard: the image as PNG (`image/png`), files as `text/uri-list`.
//!
//! An X11 clipboard holds no data: the app that copied hands it to each
//! app that pastes. So Shuttercrab keeps its clipboard for as long as it
//! runs; after it quits, the desktop keeps the last copy (GNOME does).

use anyhow::{Context, Result};
use arboard::{Clipboard, ImageData};
use std::{
    borrow::Cow,
    path::PathBuf,
    sync::{Mutex, PoisonError},
};

/// The clipboard, opened at the first copy and kept.
static CLIPBOARD: Mutex<Option<Clipboard>> = Mutex::new(None);

fn with_clipboard(copy: impl FnOnce(&mut Clipboard) -> Result<(), arboard::Error>) -> Result<()> {
    let mut held = CLIPBOARD.lock().unwrap_or_else(PoisonError::into_inner);
    let clipboard = match &mut *held {
        Some(clipboard) => clipboard,
        None => held.insert(Clipboard::new().context("opening the clipboard")?),
    };
    copy(clipboard).context("copying to the clipboard")
}

/// Straight-alpha RGBA, `width` × `height`; arboard offers it as PNG.
pub(super) fn copy_image(rgba: &[u8], width: u32, height: u32) -> Result<()> {
    with_clipboard(|clipboard| {
        clipboard.set_image(ImageData {
            width: width as usize,
            height: height as usize,
            bytes: Cow::Borrowed(rgba),
        })
    })
}

/// Files, as a file manager copies them: they paste into a folder or a
/// chat.
pub(super) fn copy_files(paths: &[PathBuf]) -> Result<()> {
    with_clipboard(|clipboard| clipboard.set().file_list(paths))
}
