//! The platform thread against real Windows.
//!
//! The clipboard test replaces the clipboard, so it is ignored by default:
//!
//!   cargo test -p framecut-platform --test platform -- --ignored
#![cfg(windows)]

use framecut_platform::{Hotkey, Platform};
use futures::executor::block_on;

#[test]
fn registers_hotkeys_and_reports_conflicts() {
    // F24 exists on no ordinary keyboard, so nothing else should own it.
    let hotkey = Hotkey::parse("Ctrl+Alt+Shift+F24").unwrap();
    let (first, _events, conflicts) = Platform::start(&[(1, hotkey)]).unwrap();
    assert!(conflicts.is_empty(), "{conflicts:?}");
    // A second registration of the same hotkey is refused and reported.
    let (_second, _events, conflicts) = Platform::start(&[(7, hotkey)]).unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].id, 7);
    drop(first);
}

#[test]
#[ignore = "replaces the clipboard"]
fn puts_png_and_bitmap_on_the_clipboard() {
    use windows::Win32::System::{
        DataExchange::{
            CloseClipboard, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
        },
        Ole::{CF_BITMAP, CF_DIB, CF_DIBV5},
    };
    let (platform, _events, _) = Platform::start(&[]).unwrap();
    let rgba = vec![200u8; 4 * 3 * 2];
    block_on(platform.copy_image(vec![0x89, b'P', b'N', b'G'], rgba, 3, 2)).unwrap();
    unsafe {
        let png = RegisterClipboardFormatW(windows::core::w!("PNG"));
        OpenClipboard(None).unwrap();
        for format in [png, CF_DIBV5.0 as u32, CF_DIB.0 as u32, CF_BITMAP.0 as u32] {
            assert!(
                IsClipboardFormatAvailable(format).is_ok(),
                "format {format}"
            );
        }
        CloseClipboard().unwrap();
    }
}

/// A captioned window's client area is smaller than the window; `cover`
/// must make it the whole requested rectangle. This is what left the
/// overlay 11 px short of the screen edges at 150%.
#[test]
fn cover_makes_the_client_area_the_whole_rectangle() {
    use framecut_platform::window::{client_bounds, cover};
    use windows::{
        Win32::UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
            WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW,
            },
        },
        core::w,
    };
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("framecut test"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            800,
            600,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let handle = hwnd.0 as isize;
        let before = client_bounds(handle).unwrap();
        assert_ne!(
            before,
            (0, 0, 800, 600),
            "a captioned window starts with a caption and borders"
        );
        // Far off-screen, so nothing flashes on the desktop.
        cover(handle, -30000, -30000, 640, 360).unwrap();
        assert_eq!(client_bounds(handle).unwrap(), (-30000, -30000, 640, 360));
        DestroyWindow(hwnd).unwrap();
    }
}
