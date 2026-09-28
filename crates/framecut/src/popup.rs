//! Framecut's borderless popup windows (the selection overlay and the
//! Capture Bar): opened at an exact physical rectangle, topmost and
//! focused, and reporting one event from their view.
//!
//! GPUI places windows in logical pixels and gives popups a frame; the
//! platform layer then makes the window a true borderless popup at the
//! physical rectangle (PRD §17, §20).

use framecut_capture::{MonitorInfo, PhysicalRect};
use framecut_platform::window as platform_window;
use futures::channel::oneshot;
use gpui_kit::{
    AnyWindowHandle, App, AsyncApp, Bounds, DisplayId, Entity, EventEmitter, Render, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, point, px, size,
};
use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use std::{cell::Cell, time::Duration};

/// How long a hidden popup lingers before it is removed.
const REMOVE_DELAY: Duration = Duration::from_millis(100);

/// An open popup.
pub struct Popup {
    window: AnyWindowHandle,
    hwnd: Option<isize>,
}

impl Popup {
    /// The window handle, for platform calls.
    pub fn hwnd(&self) -> Option<isize> {
        self.hwnd
    }

    /// Hide the popup at once and remove it a moment later. GPUI handles the
    /// deactivation that hiding causes on a later turn of the main thread,
    /// and logs "window not found" if the window is gone by then.
    pub fn close(self, cx: &mut AsyncApp) {
        if let Some(hwnd) = self.hwnd {
            platform_window::hide(hwnd);
        }
        let window = self.window;
        cx.spawn(async move |cx| {
            cx.background_executor().timer(REMOVE_DELAY).await;
            let _ = window.update(cx, |_, window, _| window.remove_window());
        })
        .detach();
    }
}

/// Open a popup covering `rect` (physical, virtual-desktop pixels) on
/// `monitor`, with the view `build` makes. Returns the popup and the first
/// event the view emits.
pub fn open<V, E>(
    monitor: &MonitorInfo,
    rect: PhysicalRect,
    cx: &mut AsyncApp,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V> + 'static,
) -> Result<(Popup, oneshot::Receiver<E>), String>
where
    V: Render + EventEmitter<E>,
    E: Clone + 'static,
{
    let (report, outcome) = oneshot::channel::<E>();
    let report = Cell::new(Some(report));
    let scale = monitor.scale_factor;
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(rect.x as f32 / scale), px(rect.y as f32 / scale)),
            size: size(
                px(rect.width as f32 / scale),
                px(rect.height as f32 / scale),
            ),
        })),
        titlebar: None,
        focus: true,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id: Some(DisplayId::new(monitor.id.0)),
        window_background: WindowBackgroundAppearance::Opaque,
        ..Default::default()
    };
    let window = cx
        .open_window(options, move |window, cx| {
            let view = build(window, cx);
            cx.subscribe(&view, move |_, event: &E, _| {
                if let Some(report) = report.take() {
                    let _ = report.send(event.clone());
                }
            })
            .detach();
            view
        })
        .map_err(|e| format!("Could not open a window: {e:#}"))?;
    // Place the window outside GPUI's update: moving it calls GPUI back,
    // which fails ("RefCell already borrowed") while the app is borrowed,
    // and GPUI would miss the new bounds.
    let hwnd = window
        .update(cx, |_, window, _| match window.window_handle() {
            Ok(handle) => match handle.as_raw() {
                RawWindowHandle::Win32(win32) => Some(win32.hwnd.get()),
                _ => None,
            },
            Err(_) => None,
        })
        .ok()
        .flatten();
    match hwnd {
        Some(hwnd) => {
            let (x, y, w, h) = (rect.x, rect.y, rect.width, rect.height);
            if let Err(e) = platform_window::cover(hwnd, x, y, w, h) {
                log::error!("could not place a popup: {e:#}");
            }
            // Views map pointer positions assuming the drawable area is
            // exactly the rectangle.
            match platform_window::client_bounds(hwnd) {
                Ok(client) if client != (x, y, w, h) => {
                    log::error!("popup area {client:?} does not match {rect:?}")
                }
                Ok(_) => {}
                Err(e) => log::error!("could not read a popup's area: {e:#}"),
            }
            platform_window::bring_to_front(hwnd);
        }
        None => log::error!("a popup has no window handle to place"),
    }
    Ok((
        Popup {
            window: window.into(),
            hwnd,
        },
        outcome,
    ))
}
