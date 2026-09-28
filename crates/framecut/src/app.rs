//! The screenshot flow (PRD §7.2, Milestone 1): hotkey → freeze the monitor
//! under the pointer → overlay → drag → release → PNG on the clipboard.
//!
//! Capture and clipboard work happens on the capture and platform threads;
//! this code only awaits them, so the GPUI main thread never blocks (§17).

use crate::overlay::{OverlayEvent, OverlayFrame, SelectionOverlay};
use framecut_capture::{Capture, FrozenFrame, monitor_under_pointer};
use framecut_platform::{Platform, PlatformEvent, window as platform_window};
use futures::{StreamExt as _, channel::mpsc::UnboundedReceiver, channel::oneshot};
use gpui_kit::{
    App, AppContext as _, AsyncApp, Bounds, DisplayId, QuitMode, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions, point, px, size,
};
use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use std::{cell::Cell, rc::Rc, time::Instant};

/// Ids for the hotkeys the platform thread registers.
pub const SCREENSHOT_HOTKEY: u32 = 1;
pub const QUIT_HOTKEY: u32 = 2;

/// Start handling platform events. Call once, inside the GPUI application.
pub fn run(
    capture: Capture,
    platform: Platform,
    events: UnboundedReceiver<PlatformEvent>,
    cx: &mut App,
) {
    // The overlay is Framecut's only window. GPUI's default on Windows quits
    // when the last window closes, which would end the app the moment a
    // selection finishes, before the clipboard is written.
    cx.set_quit_mode(QuitMode::Explicit);
    let platform = Rc::new(platform);
    let busy = Rc::new(Cell::new(false));
    cx.spawn(async move |cx| {
        let mut events = events;
        while let Some(event) = events.next().await {
            match event {
                PlatformEvent::Hotkey(SCREENSHOT_HOTKEY) => {
                    // One capture at a time; a second press while the
                    // overlay is up is ignored.
                    if busy.replace(true) {
                        continue;
                    }
                    let (capture, platform, busy) =
                        (capture.clone(), platform.clone(), busy.clone());
                    cx.spawn(async move |cx| {
                        if let Err(message) = screenshot(&capture, &platform, cx).await {
                            eprintln!("framecut: {message}");
                        }
                        busy.set(false);
                    })
                    .detach();
                }
                PlatformEvent::Hotkey(QUIT_HOTKEY) => {
                    cx.update(|cx| cx.quit());
                }
                PlatformEvent::Hotkey(_) => {}
            }
        }
    })
    .detach();
}

async fn screenshot(
    capture: &Capture,
    platform: &Platform,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let pressed = Instant::now();
    let monitor = monitor_under_pointer().ok_or("no monitor is under the pointer")?;
    let frame = capture
        .freeze_monitor(monitor)
        .await
        .map_err(|e| e.to_string())?;
    let frozen = pressed.elapsed();
    let (width, height) = frame.size();
    let info = frame.monitor().clone();
    let overlay_frame = OverlayFrame::from_bgra(
        width,
        height,
        info.scale_factor,
        frame.preview_bgra().to_vec(),
    );

    let (report, outcome) = oneshot::channel::<OverlayEvent>();
    let report = Cell::new(Some(report));
    let scale = info.scale_factor;
    let b = info.bounds;
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(b.x as f32 / scale), px(b.y as f32 / scale)),
            size: size(px(b.width as f32 / scale), px(b.height as f32 / scale)),
        })),
        titlebar: None,
        focus: true,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id: Some(DisplayId::new(info.id.0)),
        window_background: WindowBackgroundAppearance::Opaque,
        ..Default::default()
    };
    let window = cx
        .open_window(options, |window, cx| {
            let view = cx.new(|cx| SelectionOverlay::new(overlay_frame, window, cx));
            cx.subscribe(&view, move |_, event: &OverlayEvent, _| {
                if let Some(report) = report.take() {
                    let _ = report.send(*event);
                }
            })
            .detach();
            // Cover the monitor's exact physical bounds, topmost, and take
            // the keyboard so Escape works at once.
            if let Ok(handle) = window.window_handle()
                && let RawWindowHandle::Win32(win32) = handle.as_raw()
            {
                let hwnd = win32.hwnd.get();
                if let Err(e) = platform_window::cover(hwnd, b.x, b.y, b.width, b.height) {
                    eprintln!("framecut: could not place the overlay: {e:#}");
                }
                // The selection maps pointer positions onto the frozen frame
                // assuming the drawable area is exactly the monitor.
                match platform_window::client_bounds(hwnd) {
                    Ok(client) if client != (b.x, b.y, b.width, b.height) => {
                        eprintln!(
                            "framecut: overlay area {client:?} does not match the monitor {b:?}"
                        )
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("framecut: could not read the overlay area: {e:#}"),
                }
                platform_window::bring_to_front(hwnd);
            }
            view
        })
        .map_err(|e| format!("could not open the overlay: {e:#}"))?;
    eprintln!(
        "framecut: overlay up {} ms after the hotkey (freeze {} ms)",
        pressed.elapsed().as_millis(),
        frozen.as_millis()
    );

    let event = outcome.await.unwrap_or(OverlayEvent::Cancelled);
    let _ = window.update(cx, |_, window, _| window.remove_window());
    match event {
        OverlayEvent::Cancelled => Ok(()),
        OverlayEvent::Selected(rect) => copy(capture, platform, &frame, rect).await,
    }
}

async fn copy(
    capture: &Capture,
    platform: &Platform,
    frame: &FrozenFrame,
    rect: framecut_capture::PhysicalRect,
) -> Result<(), String> {
    let released = Instant::now();
    let shot = capture
        .screenshot(frame, rect)
        .await
        .map_err(|e| e.to_string())?;
    let (width, height) = (shot.width, shot.height);
    platform
        .copy_image(shot.png, shot.rgba, width, height)
        .await
        .map_err(|e| format!("could not copy the screenshot: {e:#}"))?;
    eprintln!(
        "framecut: {width}×{height} screenshot on the clipboard {} ms after release",
        released.elapsed().as_millis()
    );
    Ok(())
}
