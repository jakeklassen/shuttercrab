//! Putting a screenshot on the Windows clipboard as PNG and as a DIB, or
//! files (a recording) as Explorer copies them.
//!
//! "PNG" is the registered format browsers, Slack, Discord and most modern
//! apps paste. `CF_DIBV5` covers everything else; Windows synthesizes
//! `CF_DIB` and `CF_BITMAP` from it.

use anyhow::{Context, Result, bail};
use std::{os::windows::ffi::OsStrExt, path::PathBuf, time::Duration};
use windows::{
    Win32::{
        Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND},
        Graphics::Gdi::{BI_BITFIELDS, BITMAPV5HEADER, LCS_GM_IMAGES},
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetOpenClipboardWindow, OpenClipboard,
                RegisterClipboardFormatW, SetClipboardData,
            },
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            Ole::{CF_DIBV5, CF_HDROP},
        },
        UI::ColorSystem::LCS_sRGB,
    },
    core::w,
};

/// How long to keep retrying while another application holds the clipboard.
const ATTEMPTS: u32 = 20;
const RETRY: Duration = Duration::from_millis(15);

/// A `CF_DIBV5` block for straight-alpha RGBA8 pixels: a `BITMAPV5HEADER`
/// (sRGB, 32-bit BGRA with bit-field masks) followed by rows bottom to top.
/// Many applications ignore a bitmap's alpha, so the bitmap is opaque:
/// partly transparent pixels (window corners) are composited over white.
/// The PNG on the clipboard keeps the transparency.
pub fn dibv5(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    if rgba.len() != (width * height * 4) as usize || width == 0 || height == 0 {
        bail!("image buffer does not match {width}x{height}");
    }
    let header = BITMAPV5HEADER {
        bV5Size: size_of::<BITMAPV5HEADER>() as u32,
        bV5Width: width as i32,
        bV5Height: height as i32,
        bV5Planes: 1,
        bV5BitCount: 32,
        bV5Compression: BI_BITFIELDS,
        bV5SizeImage: width * height * 4,
        bV5RedMask: 0x00FF_0000,
        bV5GreenMask: 0x0000_FF00,
        bV5BlueMask: 0x0000_00FF,
        bV5AlphaMask: 0xFF00_0000,
        bV5CSType: LCS_sRGB.0 as u32,
        bV5Intent: LCS_GM_IMAGES as u32,
        ..Default::default()
    };
    let mut out = Vec::with_capacity(size_of::<BITMAPV5HEADER>() + rgba.len());
    // SAFETY: BITMAPV5HEADER is a plain repr(C) struct of integers.
    out.extend_from_slice(unsafe {
        std::slice::from_raw_parts(
            (&header as *const BITMAPV5HEADER).cast::<u8>(),
            size_of::<BITMAPV5HEADER>(),
        )
    });
    let row = (width * 4) as usize;
    for y in (0..height as usize).rev() {
        for px in rgba[y * row..(y + 1) * row].as_chunks::<4>().0 {
            let a = px[3] as u32;
            let over_white = |c: u8| ((c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
            out.extend_from_slice(&[over_white(px[2]), over_white(px[1]), over_white(px[0]), 255]);
        }
    }
    Ok(out)
}

/// Copy `bytes` into a movable global block the clipboard can own.
fn global(bytes: &[u8]) -> Result<HGLOBAL> {
    unsafe {
        let block = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).context("GlobalAlloc failed")?;
        let target = GlobalLock(block);
        if target.is_null() {
            let _ = GlobalFree(Some(block));
            bail!("GlobalLock failed");
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), target.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(block);
        Ok(block)
    }
}

/// The program that has the clipboard open, for the log. Windows knows it
/// only if it opened the clipboard with a window of its own.
fn holder() -> String {
    use windows::{
        Win32::{
            System::Threading::{
                OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
            UI::WindowsAndMessaging::GetWindowThreadProcessId,
        },
        core::PWSTR,
    };
    let Ok(window) = (unsafe { GetOpenClipboardWindow() }) else {
        return "held without a window, so the program is unknown".into();
    };
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    let name = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .ok()
        .and_then(|process| {
            let mut buffer = [0u16; 260];
            let mut len = buffer.len() as u32;
            let found = unsafe {
                QueryFullProcessImageNameW(
                    process,
                    PROCESS_NAME_WIN32,
                    PWSTR(buffer.as_mut_ptr()),
                    &mut len,
                )
            }
            .is_ok();
            let _ = unsafe { windows::Win32::Foundation::CloseHandle(process) };
            found.then(|| String::from_utf16_lossy(&buffer[..len as usize]))
        });
    match name {
        Some(path) => format!(
            "{}, process {pid}",
            path.rsplit(['\\', '/']).next().unwrap_or(&path)
        ),
        None => format!("process {pid}"),
    }
}

/// Replace the clipboard contents with the image. `owner` must be a window
/// on the calling thread: with no owner, Windows refuses the data.
pub(crate) fn write(owner: HWND, png: &[u8], rgba: &[u8], width: u32, height: u32) -> Result<()> {
    let dib = dibv5(rgba, width, height)?;
    let png_format = unsafe { RegisterClipboardFormatW(w!("PNG")) };
    if png_format == 0 {
        bail!("could not register the PNG clipboard format");
    }
    replace(
        owner,
        &[(png_format, png), (CF_DIBV5.0 as u32, dib.as_slice())],
    )
}

/// Replace the clipboard contents with files, as Explorer's Copy does:
/// they paste into Explorer as copies, and into chat apps as attachments.
pub(crate) fn write_files(owner: HWND, paths: &[PathBuf]) -> Result<()> {
    let effect_format = unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) };
    if effect_format == 0 {
        bail!("could not register the drop effect clipboard format");
    }
    let files = drop_files(paths);
    replace(
        owner,
        &[
            (CF_HDROP.0 as u32, files.as_slice()),
            (effect_format, &DROPEFFECT_COPY.to_le_bytes()),
        ],
    )
}

/// What "Preferred DropEffect" holds for a copy, not a move.
const DROPEFFECT_COPY: u32 = 1;

/// A `CF_HDROP` block: a `DROPFILES` header (the list's offset, a point and
/// two flags, the last saying the paths are UTF-16), then each path
/// null-terminated, then one more null.
fn drop_files(paths: &[PathBuf]) -> Vec<u8> {
    const HEADER: u32 = 20;
    let mut out = Vec::new();
    for value in [HEADER, 0, 0, 0, 1] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    for path in paths {
        for unit in path.as_os_str().encode_wide().chain([0]) {
            out.extend_from_slice(&unit.to_le_bytes());
        }
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// Open the clipboard (retrying while another application holds it),
/// empty it, and put each `(format, bytes)` on it.
fn replace(owner: HWND, data: &[(u32, &[u8])]) -> Result<()> {
    let mut opened = false;
    for _ in 0..ATTEMPTS {
        if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(RETRY);
    }
    if !opened {
        bail!(
            "another application is holding the clipboard ({})",
            holder()
        );
    }
    let result = (|| -> Result<()> {
        unsafe { EmptyClipboard() }.context("EmptyClipboard failed")?;
        for &(format, bytes) in data {
            let block = global(bytes)?;
            // On success the clipboard owns the block; on failure we still do.
            if let Err(e) = unsafe { SetClipboardData(format, Some(HANDLE(block.0))) } {
                let _ = unsafe { GlobalFree(Some(block)) };
                return Err(e).context("SetClipboardData failed");
            }
        }
        Ok(())
    })();
    let _ = unsafe { CloseClipboard() };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dib_is_bottom_up_bgra_with_an_srgb_v5_header() {
        // 2x2: top row red, green; bottom row blue, white.
        let rgba = [
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let dib = dibv5(&rgba, 2, 2).unwrap();
        let header = size_of::<BITMAPV5HEADER>();
        assert_eq!(header, 124);
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 124);
        assert_eq!(i32::from_le_bytes(dib[4..8].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(dib[14..16].try_into().unwrap()), 32);
        assert_eq!(dib.len(), header + 16);
        // Bottom row first, in BGRA, opaque.
        assert_eq!(
            &dib[header..header + 8],
            [255, 0, 0, 255, 255, 255, 255, 255]
        );
        assert_eq!(&dib[header + 8..], [0, 0, 255, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn transparent_pixels_are_composited_over_white() {
        // Black at half coverage, then fully transparent black.
        let dib = dibv5(&[0, 0, 0, 128, 0, 0, 0, 0], 2, 1).unwrap();
        let pixels = &dib[size_of::<BITMAPV5HEADER>()..];
        assert_eq!(pixels, [127, 127, 127, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn files_are_a_dropfiles_header_and_wide_paths_ending_in_two_nulls() {
        let block = drop_files(&[PathBuf::from(r"C:\a.mp4"), PathBuf::from("b")]);
        let word = |i: usize| u32::from_le_bytes(block[i..i + 4].try_into().unwrap());
        // The paths start after the header, and are wide.
        assert_eq!((word(0), word(16)), (20, 1));
        let units: Vec<u16> = block[20..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        assert_eq!(
            String::from_utf16(&units).unwrap(),
            "C:\\a.mp4\u{0}b\u{0}\u{0}"
        );
    }

    #[test]
    fn dib_rejects_mismatched_buffers() {
        assert!(dibv5(&[0; 12], 2, 2).is_err());
        assert!(dibv5(&[], 0, 0).is_err());
    }
}
