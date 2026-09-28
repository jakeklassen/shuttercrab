//! The screenshot flow (PRD §7.2): hotkey or tray → freeze the monitor under
//! the pointer → overlay → drag → release → PNG on the clipboard and, with
//! auto-save on, in the output folder.
//!
//! Capture, clipboard and file work happen on the capture thread, the
//! platform thread and GPUI's background executor; this code only awaits
//! them, so the GPUI main thread never blocks (§17).

use crate::{
    files,
    overlay::{OverlayEvent, OverlayFrame, SelectionOverlay},
    settings::{self, Settings},
};
use chrono::{Local, NaiveDateTime};
use framecut_capture::{Capture, FrozenFrame, PhysicalRect, monitor_under_pointer};
use framecut_platform::{MenuItem, Platform, PlatformEvent, window as platform_window};
use futures::{StreamExt as _, channel::mpsc::UnboundedReceiver, channel::oneshot};
use gpui_kit::{
    App, AppContext as _, AsyncApp, Bounds, DisplayId, QuitMode, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions, point, px, size,
};
use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

/// Ids for the hotkeys the platform thread registers.
pub const SCREENSHOT_HOTKEY: u32 = 1;
pub const QUIT_HOTKEY: u32 = 2;

/// Ids for the tray menu's items.
pub const MENU_SCREENSHOT: u32 = 1;
pub const MENU_OPEN_FOLDER: u32 = 2;
pub const MENU_AUTO_SAVE: u32 = 3;
pub const MENU_QUIT: u32 = 4;

/// A capture started from the tray waits this long, so the tray menu or
/// flyout has closed before the screen is frozen.
const TRAY_DELAY: Duration = Duration::from_millis(250);

/// How long a hidden overlay lingers before it is removed.
const REMOVE_DELAY: Duration = Duration::from_millis(100);

/// The tray icon's context menu for the current settings.
pub fn tray_menu(settings: &Settings) -> Vec<MenuItem> {
    vec![
        // A tab right-aligns the rest of the label, like a menu accelerator.
        MenuItem::item(
            MENU_SCREENSHOT,
            format!("Take screenshot\t{}", settings.screenshot_hotkey),
        ),
        MenuItem::item(MENU_OPEN_FOLDER, "Open screenshots folder"),
        MenuItem::Separator,
        MenuItem::Item {
            id: MENU_AUTO_SAVE,
            label: "Save screenshots to the folder".into(),
            enabled: true,
            checked: settings.auto_save,
        },
        MenuItem::Separator,
        MenuItem::item(MENU_QUIT, "Quit Framecut"),
    ]
}

/// Everything the running app shares between its tasks.
pub struct Framecut {
    pub capture: Capture,
    pub platform: Platform,
    pub settings: Settings,
    /// Where settings are saved; `None` if there is no config folder.
    pub settings_path: Option<PathBuf>,
}

struct State {
    capture: Capture,
    platform: Platform,
    settings: RefCell<Settings>,
    settings_path: Option<PathBuf>,
    /// One capture at a time: a second request while one is running is
    /// ignored.
    busy: Cell<bool>,
}

/// Start handling platform events. Call once, inside the GPUI application.
pub fn run(framecut: Framecut, events: UnboundedReceiver<PlatformEvent>, cx: &mut App) {
    // Framecut lives in the tray and has no main window. GPUI's default on
    // Windows quits when the last window closes, which would end the app
    // the moment a selection finishes, before the clipboard is written.
    cx.set_quit_mode(QuitMode::Explicit);
    let state = Rc::new(State {
        capture: framecut.capture,
        platform: framecut.platform,
        settings: RefCell::new(framecut.settings),
        settings_path: framecut.settings_path,
        busy: Cell::new(false),
    });
    cx.spawn(async move |cx| {
        let mut events = events;
        while let Some(event) = events.next().await {
            match event {
                PlatformEvent::Hotkey(SCREENSHOT_HOTKEY) => start_screenshot(&state, None, cx),
                PlatformEvent::TrayActivated | PlatformEvent::TrayCommand(MENU_SCREENSHOT) => {
                    start_screenshot(&state, Some(TRAY_DELAY), cx)
                }
                PlatformEvent::TrayCommand(MENU_OPEN_FOLDER) => open_folder(&state, cx),
                PlatformEvent::TrayCommand(MENU_AUTO_SAVE) => toggle_auto_save(&state, cx),
                PlatformEvent::Hotkey(QUIT_HOTKEY) | PlatformEvent::TrayCommand(MENU_QUIT) => {
                    log::info!("quitting");
                    cx.update(|cx| cx.quit());
                }
                PlatformEvent::AnotherInstance => {
                    log::info!("Framecut was started again; this instance keeps running");
                    let hotkey = state.settings.borrow().screenshot_hotkey.clone();
                    state.platform.notify(
                        "Framecut is already running",
                        format!("Press {hotkey} to take a screenshot, or click the tray icon."),
                    );
                }
                PlatformEvent::Hotkey(_) | PlatformEvent::TrayCommand(_) => {}
            }
        }
    })
    .detach();
}

fn start_screenshot(state: &Rc<State>, delay: Option<Duration>, cx: &mut AsyncApp) {
    if state.busy.replace(true) {
        return;
    }
    let state = state.clone();
    cx.spawn(async move |cx| {
        if let Some(delay) = delay {
            cx.background_executor().timer(delay).await;
        }
        if let Err(message) = screenshot(&state, cx).await {
            log::error!("{message}");
            state.platform.notify("Screenshot failed", message);
        }
        state.busy.set(false);
    })
    .detach();
}

fn open_folder(state: &State, cx: &mut AsyncApp) {
    let dir = state.settings.borrow().output_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::error!("could not create {}: {e}", dir.display());
        state
            .platform
            .notify("Could not open the folder", e.to_string());
        return;
    }
    cx.update(|cx| cx.open_with_system(&dir));
}

fn toggle_auto_save(state: &State, cx: &mut AsyncApp) {
    let settings = {
        let mut settings = state.settings.borrow_mut();
        settings.auto_save = !settings.auto_save;
        settings.clone()
    };
    log::info!(
        "auto-save {}",
        if settings.auto_save { "on" } else { "off" }
    );
    state.platform.set_tray_menu(tray_menu(&settings));
    if let Some(path) = state.settings_path.clone() {
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = settings::save(&path, &settings) {
                    log::error!("could not save settings: {e:#}");
                }
            })
            .detach();
    }
}

async fn screenshot(state: &State, cx: &mut AsyncApp) -> Result<(), String> {
    let pressed = Instant::now();
    let taken_at = Local::now().naive_local();
    let monitor = monitor_under_pointer().ok_or("No monitor is under the pointer.")?;
    let frame = state
        .capture
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
            view
        })
        .map_err(|e| format!("Could not open the overlay: {e:#}"))?;
    // Cover the monitor's exact physical bounds, topmost, and take the
    // keyboard so Escape works at once. This must happen outside GPUI's
    // update: moving the window calls GPUI back, which fails ("RefCell
    // already borrowed") while the app is borrowed, and GPUI would miss the
    // new bounds.
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
    if let Some(hwnd) = hwnd {
        if let Err(e) = platform_window::cover(hwnd, b.x, b.y, b.width, b.height) {
            log::error!("could not place the overlay: {e:#}");
        }
        // The selection maps pointer positions onto the frozen frame
        // assuming the drawable area is exactly the monitor.
        match platform_window::client_bounds(hwnd) {
            Ok(client) if client != (b.x, b.y, b.width, b.height) => {
                log::error!("overlay area {client:?} does not match the monitor {b:?}")
            }
            Ok(_) => {}
            Err(e) => log::error!("could not read the overlay area: {e:#}"),
        }
        platform_window::bring_to_front(hwnd);
    } else {
        log::error!("the overlay has no window handle to place");
    }
    log::info!(
        "overlay up {} ms after the request (freeze {} ms)",
        pressed.elapsed().as_millis(),
        frozen.as_millis()
    );

    let event = outcome.await.unwrap_or(OverlayEvent::Cancelled);
    // Hide at once, remove a moment later: GPUI handles the deactivation
    // that hiding causes on a later turn of the main thread, and logs
    // "window not found" if the window is gone by then.
    if let Some(hwnd) = hwnd {
        platform_window::hide(hwnd);
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(REMOVE_DELAY).await;
        let _ = window.update(cx, |_, window, _| window.remove_window());
    })
    .detach();
    match event {
        OverlayEvent::Cancelled => {
            log::info!("selection cancelled");
            Ok(())
        }
        OverlayEvent::Selected(rect) => deliver(state, &frame, rect, taken_at, cx).await,
    }
}

/// Encode the selection, then copy and save it as the settings say.
async fn deliver(
    state: &State,
    frame: &FrozenFrame,
    rect: PhysicalRect,
    taken_at: NaiveDateTime,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let released = Instant::now();
    let settings = state.settings.borrow().clone();
    let shot = state
        .capture
        .screenshot(frame, rect)
        .await
        .map_err(|e| e.to_string())?;
    let (width, height) = (shot.width, shot.height);

    // Start the file write first; it runs while the clipboard is written.
    let saved = settings.auto_save.then(|| {
        let (dir, png) = (settings.output_dir(), shot.png.clone());
        cx.background_executor()
            .spawn(async move { files::save_screenshot(&dir, taken_at, &png) })
    });
    let copied = if settings.copy_to_clipboard {
        let result = state
            .platform
            .copy_image(shot.png, shot.rgba, width, height)
            .await;
        if result.is_ok() {
            log::info!(
                "{width}×{height} screenshot on the clipboard {} ms after release",
                released.elapsed().as_millis()
            );
        }
        Some(result)
    } else {
        None
    };
    let saved = match saved {
        Some(task) => Some(task.await),
        None => None,
    };
    if let Some(Ok(path)) = &saved {
        log::info!(
            "saved {} {} ms after release",
            path.file_name().unwrap_or_default().to_string_lossy(),
            released.elapsed().as_millis()
        );
    }

    match (copied, saved) {
        (Some(Err(copy)), Some(Err(save))) => Err(format!(
            "Could not copy or save the screenshot: {copy:#}; {save:#}"
        )),
        (Some(Err(copy)), Some(Ok(_))) => Err(format!(
            "The screenshot was saved, but could not be copied: {copy:#}"
        )),
        (Some(Err(copy)), None) => Err(format!("Could not copy the screenshot: {copy:#}")),
        (Some(Ok(())), Some(Err(save))) => Err(format!(
            "The screenshot was copied, but could not be saved: {save:#}"
        )),
        (None, Some(Err(save))) => Err(format!("Could not save the screenshot: {save:#}")),
        (None, None) => {
            log::warn!("screenshot discarded: copying and saving are both off");
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto_save_checked(menu: &[MenuItem]) -> bool {
        menu.iter()
            .find_map(|item| match item {
                MenuItem::Item {
                    id: MENU_AUTO_SAVE,
                    checked,
                    ..
                } => Some(*checked),
                _ => None,
            })
            .expect("the menu has the auto-save item")
    }

    #[test]
    fn the_tray_menu_follows_the_settings() {
        let settings = Settings::default();
        let menu = tray_menu(&settings);
        assert!(auto_save_checked(&menu));
        assert!(menu.contains(&MenuItem::item(
            MENU_SCREENSHOT,
            "Take screenshot\tCtrl+Alt+S"
        )));
        let off = Settings {
            auto_save: false,
            ..settings
        };
        assert!(!auto_save_checked(&tray_menu(&off)));
    }
}
