//! The platform thread against real Windows.
//!
//! The clipboard test replaces the clipboard, so it is ignored by default:
//!
//!   cargo test -p shuttercrab-platform --test platform -- --ignored
#![cfg(windows)]

use futures::executor::block_on;
use shuttercrab_platform::{Hotkey, Platform};
use shuttercrab_types::WindowId;

#[test]
fn registers_hotkeys_and_reports_conflicts() {
    // F24 exists on no ordinary keyboard, so nothing else should own it.
    let hotkey = Hotkey::parse("Ctrl+Alt+Shift+F24").unwrap();
    let (first, _events, conflicts) = Platform::start(&[(1, hotkey)], None).unwrap();
    assert!(conflicts.is_empty(), "{conflicts:?}");
    // A second registration of the same hotkey is refused and reported.
    let (_second, _events, conflicts) = Platform::start(&[(7, hotkey)], None).unwrap();
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
    let (platform, _events, _) = Platform::start(&[], None).unwrap();
    let rgba = vec![200u8; 4 * 3 * 2];
    let png = std::sync::Arc::new(vec![0x89, b'P', b'N', b'G']);
    block_on(platform.copy_image(png, std::sync::Arc::new(rgba), 3, 2)).unwrap();
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
    use shuttercrab_platform::window::{client_bounds, cover};
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
            w!("shuttercrab test"),
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
        let handle = WindowId::from_raw(hwnd.0 as u64);
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

#[test]
fn shuttercrabs_own_windows_are_targets_unless_excluded_from_capture() {
    use shuttercrab_platform::{
        targets::visible_windows,
        window::{cover, exclude_from_capture},
    };
    use windows::{
        Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_POPUP,
        },
        core::w,
    };
    unsafe {
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("shuttercrab target test"),
            WS_POPUP,
            0,
            0,
            100,
            100,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let handle = WindowId::from_raw(hwnd.0 as u64);
        // Shown far off-screen, so nothing flashes on the desktop.
        cover(handle, -30000, -30000, 320, 200).unwrap();
        let listed = || visible_windows().iter().any(|w| w.id == handle);
        assert!(listed(), "an ordinary window of this process is a target");
        exclude_from_capture(handle).unwrap();
        assert!(!listed(), "a window excluded from capture is not");
        DestroyWindow(hwnd).unwrap();
    }
}

#[test]
fn the_recording_border_lets_the_pointer_through_and_stays_out_of_captures() {
    use shuttercrab_platform::frame::{Frame, FrameStyle, Rect};
    use windows::Win32::{
        Foundation::{HWND, RECT},
        UI::WindowsAndMessaging::{
            GWL_EXSTYLE, GetWindowDisplayAffinity, GetWindowLongW, GetWindowRect, IsWindowVisible,
            WDA_EXCLUDEFROMCAPTURE, WS_EX_NOACTIVATE, WS_EX_TRANSPARENT,
        },
    };
    // A pretend monitor far off-screen, so nothing flashes on the desktop.
    let bounds = Rect::new(-30000, -30000, 1000, 800);
    let area = Rect::new(-29900, -29900, 400, 300);
    let style = FrameStyle {
        thickness: 3,
        dash: 12,
        gap: 8,
    };
    let (mut frame, missing) = Frame::show(area, bounds, style, [0xE5, 0x48, 0x4D]).unwrap();
    assert_eq!(missing, 0);
    let windows = frame.windows();
    assert_eq!(windows.len(), 4);
    for handle in windows {
        let hwnd = HWND(handle.raw() as _);
        unsafe {
            assert!(IsWindowVisible(hwnd).as_bool());
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            assert_ne!(ex & WS_EX_TRANSPARENT.0, 0, "the pointer passes through");
            assert_ne!(ex & WS_EX_NOACTIVATE.0, 0, "it never takes the keyboard");
            let mut affinity = 0;
            GetWindowDisplayAffinity(hwnd, &mut affinity).unwrap();
            assert_eq!(affinity, WDA_EXCLUDEFROMCAPTURE.0);
        }
    }
    frame.recolor([0xF5, 0xA5, 0x24]).unwrap();
    // Moved and resized, as a recorded window is: each strip goes where a
    // new border's would.
    let moved = Rect::new(-29800, -29850, 500, 250);
    frame.place(moved, bounds).unwrap();
    let expected = shuttercrab_platform::frame::strips(moved, bounds, 3);
    for (handle, (want, _)) in frame.windows().into_iter().zip(expected) {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(HWND(handle.raw() as _), &mut rect).unwrap() };
        let got = Rect::new(
            rect.left,
            rect.top,
            (rect.right - rect.left) as u32,
            (rect.bottom - rect.top) as u32,
        );
        assert_eq!(got, want);
    }
    drop(frame);
}
