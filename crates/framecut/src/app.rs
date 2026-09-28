//! The screenshot flows (PRD §7.2–7.5). The Capture Bar hotkey or the tray
//! icon opens the Capture Bar, which asks for Area, Window or Display; the
//! screenshot hotkey goes straight to Area. Area and Window freeze the
//! monitor under the pointer and show the overlay; Display captures the
//! monitor at once. The PNG goes to the clipboard and, with auto-save on,
//! the output folder.
//!
//! Capture, clipboard and file work happen on the capture thread, the
//! platform thread and GPUI's background executor; this code only awaits
//! them, so the GPUI main thread never blocks (§17).

use crate::{
    capture_bar::{BAR_HEIGHT, BAR_WIDTH, CaptureBar, CaptureBarEvent, CaptureTarget},
    files,
    overlay::{Mode, OverlayEvent, OverlayFrame, SelectionOverlay},
    popup,
    selection::ScreenWindow,
    settings::{self, Settings},
    thumbnail::{self, Thumbnail, ThumbnailEvent},
};
use chrono::{Local, NaiveDateTime};
use framecut_capture::{
    Capture, MonitorId, MonitorInfo, PhysicalRect, Screenshot, monitor_under_pointer,
};
use framecut_platform::{
    MenuItem, Platform, PlatformEvent, drag::DragImage, targets, window as platform_window,
};
use futures::{StreamExt as _, channel::mpsc::UnboundedReceiver};
use gpui_kit::{App, AppContext as _, AsyncApp, QuitMode};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

/// Ids for the hotkeys the platform thread registers.
pub const SCREENSHOT_HOTKEY: u32 = 1;
pub const QUIT_HOTKEY: u32 = 2;
pub const CAPTURE_BAR_HOTKEY: u32 = 3;

/// Ids for the tray menu's items.
pub const MENU_SCREENSHOT: u32 = 1;
pub const MENU_OPEN_FOLDER: u32 = 2;
pub const MENU_AUTO_SAVE: u32 = 3;
pub const MENU_QUIT: u32 = 4;
pub const MENU_CAPTURE_BAR: u32 = 5;

/// A capture started from the tray menu waits this long, so the menu has
/// closed before the screen is frozen.
const TRAY_DELAY: Duration = Duration::from_millis(250);

/// The Capture Bar's distance from the top of the monitor, logical pixels.
const BAR_TOP: f32 = 24.0;

/// The thumbnail's distance from the work area's edges, logical pixels.
const THUMBNAIL_MARGIN: f32 = 16.0;

/// The tray icon's context menu for the current settings.
pub fn tray_menu(settings: &Settings) -> Vec<MenuItem> {
    vec![
        // A tab right-aligns the rest of the label, like a menu accelerator.
        MenuItem::item(
            MENU_CAPTURE_BAR,
            format!("Capture Bar\t{}", settings.capture_bar_hotkey),
        ),
        MenuItem::item(
            MENU_SCREENSHOT,
            format!("Screenshot an area\t{}", settings.screenshot_hotkey),
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
    /// One capture at a time: a second request while the Capture Bar or
    /// the overlay is up is ignored.
    busy: Cell<bool>,
    /// The thumbnail on screen, and a count that tells its handler whether
    /// a newer one has replaced it.
    thumbnail: RefCell<Option<popup::Popup>>,
    thumbnail_generation: Cell<u64>,
}

impl State {
    /// Change the settings and save them in the background.
    fn update_settings(&self, change: impl FnOnce(&mut Settings), cx: &mut AsyncApp) -> Settings {
        let settings = {
            let mut settings = self.settings.borrow_mut();
            change(&mut settings);
            settings.clone()
        };
        if let Some(path) = self.settings_path.clone() {
            let saved = settings.clone();
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = settings::save(&path, &saved) {
                        log::error!("could not save settings: {e:#}");
                    }
                })
                .detach();
        }
        settings
    }
}

/// What a request starts with.
#[derive(Clone, Copy, Debug)]
enum Start {
    CaptureBar,
    Target(CaptureTarget),
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
        thumbnail: RefCell::new(None),
        thumbnail_generation: Cell::new(0),
    });
    cx.spawn(async move |cx| {
        let mut events = events;
        while let Some(event) = events.next().await {
            match event {
                PlatformEvent::Hotkey(SCREENSHOT_HOTKEY) => {
                    start(&state, Start::Target(CaptureTarget::Area), None, cx)
                }
                PlatformEvent::Hotkey(CAPTURE_BAR_HOTKEY)
                | PlatformEvent::TrayActivated
                | PlatformEvent::TrayCommand(MENU_CAPTURE_BAR) => {
                    start(&state, Start::CaptureBar, None, cx)
                }
                PlatformEvent::TrayCommand(MENU_SCREENSHOT) => start(
                    &state,
                    Start::Target(CaptureTarget::Area),
                    Some(TRAY_DELAY),
                    cx,
                ),
                PlatformEvent::TrayCommand(MENU_OPEN_FOLDER) => open_folder(&state, cx),
                PlatformEvent::TrayCommand(MENU_AUTO_SAVE) => {
                    let settings = state.update_settings(|s| s.auto_save = !s.auto_save, cx);
                    log::info!(
                        "auto-save {}",
                        if settings.auto_save { "on" } else { "off" }
                    );
                    state.platform.set_tray_menu(tray_menu(&settings));
                }
                PlatformEvent::Hotkey(QUIT_HOTKEY) | PlatformEvent::TrayCommand(MENU_QUIT) => {
                    log::info!("quitting");
                    cx.update(|cx| cx.quit());
                }
                PlatformEvent::AnotherInstance => {
                    log::info!("Framecut was started again; this instance keeps running");
                    let hotkey = state.settings.borrow().capture_bar_hotkey.clone();
                    state.platform.notify(
                        "Framecut is already running",
                        format!("Press {hotkey} or click the tray icon to capture."),
                    );
                }
                PlatformEvent::Hotkey(_) | PlatformEvent::TrayCommand(_) => {}
            }
        }
    })
    .detach();
}

fn start(state: &Rc<State>, what: Start, delay: Option<Duration>, cx: &mut AsyncApp) {
    if state.busy.replace(true) {
        return;
    }
    let state = state.clone();
    cx.spawn(async move |cx| {
        if let Some(delay) = delay {
            cx.background_executor().timer(delay).await;
        }
        let pressed = Instant::now();
        let result = match monitor_under_pointer() {
            None => Err("No monitor is under the pointer.".to_string()),
            Some(monitor) => match what {
                Start::CaptureBar => capture_bar(&state, monitor, pressed, cx).await,
                Start::Target(target) => capture(&state, target, monitor, pressed, cx).await,
            },
        };
        if let Err(message) = result {
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

/// Where the Capture Bar goes: centred near the top of `monitor`.
pub fn bar_rect(monitor: &MonitorInfo) -> PhysicalRect {
    let (b, scale) = (monitor.bounds, monitor.scale_factor);
    let width = (BAR_WIDTH * scale).round() as u32;
    let height = (BAR_HEIGHT * scale).round() as u32;
    PhysicalRect::new(
        b.x + (b.width.saturating_sub(width) / 2) as i32,
        b.y + (BAR_TOP * scale).round() as i32,
        width,
        height,
    )
}

async fn capture_bar(
    state: &Rc<State>,
    monitor: MonitorId,
    pressed: Instant,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let monitors = state
        .capture
        .list_monitors()
        .await
        .map_err(|e| e.to_string())?;
    let info = monitors
        .into_iter()
        .find(|m| m.id == monitor)
        .ok_or("The monitor under the pointer is gone.")?;
    let last = state.settings.borrow().last_target;
    let (bar, outcome) = popup::open(&info, bar_rect(&info), true, cx, move |window, cx| {
        cx.new(|cx| CaptureBar::new(last, window, cx))
    })?;
    if let Some(hwnd) = bar.hwnd() {
        platform_window::round_corners(hwnd);
        // Never part of a screenshot, even if the screen is frozen while
        // the bar is still fading out. FRAMECUT_CAPTURABLE_UI keeps it
        // capturable, for screenshots of Framecut itself.
        if std::env::var_os("FRAMECUT_CAPTURABLE_UI").is_none()
            && let Err(e) = platform_window::exclude_from_capture(hwnd)
        {
            log::warn!("could not exclude the Capture Bar from capture: {e:#}");
        }
    }
    log::info!(
        "Capture Bar up {} ms after the request",
        pressed.elapsed().as_millis()
    );
    let mut outcome = outcome;
    let event = outcome.next().await.unwrap_or(CaptureBarEvent::Dismissed);
    bar.close(cx);
    match event {
        CaptureBarEvent::Dismissed => {
            log::info!("Capture Bar dismissed");
            Ok(())
        }
        CaptureBarEvent::Chosen(target) => {
            log::info!("Capture Bar: {target:?}");
            state.update_settings(|s| s.last_target = target, cx);
            capture(state, target, monitor, Instant::now(), cx).await
        }
    }
}

/// Capture `target` on `monitor`.
async fn capture(
    state: &Rc<State>,
    target: CaptureTarget,
    monitor: MonitorId,
    pressed: Instant,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let taken_at = Local::now().naive_local();
    let frame = state
        .capture
        .freeze_monitor(monitor)
        .await
        .map_err(|e| e.to_string())?;
    let (width, height) = frame.size();
    let whole = PhysicalRect::new(0, 0, width, height);
    let mode = match target {
        CaptureTarget::Display => {
            let shot = state.capture.screenshot(&frame, whole).await;
            return deliver(
                state,
                shot.map_err(|e| e.to_string())?,
                frame.monitor(),
                pressed,
                taken_at,
                cx,
            )
            .await;
        }
        CaptureTarget::Area => Mode::Area,
        CaptureTarget::Window => Mode::Window,
    };
    let frozen = pressed.elapsed();
    let info = frame.monitor().clone();
    let windows = screen_windows(info.bounds);
    let snap_to_windows = state.settings.borrow().snap_to_windows;
    let overlay_frame = OverlayFrame::from_bgra(
        width,
        height,
        info.scale_factor,
        frame.preview_bgra().to_vec(),
    );
    let (overlay, outcome) = popup::open(&info, info.bounds, true, cx, move |window, cx| {
        cx.new(|cx| {
            SelectionOverlay::new(overlay_frame, window, cx)
                .with_windows(windows, snap_to_windows)
                .with_mode(mode)
        })
    })?;
    log::info!(
        "overlay up {} ms after the request (freeze {} ms)",
        pressed.elapsed().as_millis(),
        frozen.as_millis()
    );

    let mut outcome = outcome;
    let event = outcome.next().await.unwrap_or(OverlayEvent::Cancelled);
    overlay.close(cx);
    let released = Instant::now();
    let shot = match event {
        OverlayEvent::Cancelled => {
            log::info!("selection cancelled");
            return Ok(());
        }
        OverlayEvent::Selected(rect) => state.capture.screenshot(&frame, rect).await,
        OverlayEvent::Display => state.capture.screenshot(&frame, whole).await,
        OverlayEvent::Window { hwnd, visible } => {
            match state.capture.capture_window(hwnd).await {
                Ok(shot) => Ok(shot),
                // Some windows refuse direct capture; what the user saw of
                // the window is the next best thing.
                Err(e) => {
                    log::warn!("direct window capture failed, cutting it from the screen: {e}");
                    state.capture.screenshot(&frame, visible).await
                }
            }
        }
    };
    deliver(
        state,
        shot.map_err(|e| e.to_string())?,
        &info,
        released,
        taken_at,
        cx,
    )
    .await
}

/// Windows on the monitor at `bounds`, front to back, relative to it. The
/// desktop is left out: over it, Window mode captures the whole display.
fn screen_windows(bounds: PhysicalRect) -> Vec<ScreenWindow> {
    let monitor = targets::Bounds {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
    };
    targets::visible_windows()
        .into_iter()
        .filter(|w| !w.desktop && w.bounds.intersect(&monitor).is_some())
        .map(|w| ScreenWindow {
            hwnd: w.hwnd,
            bounds: PhysicalRect::new(
                w.bounds.x - bounds.x,
                w.bounds.y - bounds.y,
                w.bounds.width,
                w.bounds.height,
            ),
        })
        .collect()
}

/// Copy and save a finished screenshot as the settings say, then show the
/// thumbnail and the notification. `released` is when the user finished
/// choosing, for the log.
async fn deliver(
    state: &Rc<State>,
    shot: Screenshot,
    monitor: &MonitorInfo,
    released: Instant,
    taken_at: NaiveDateTime,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let settings = state.settings.borrow().clone();
    let (width, height) = (shot.width, shot.height);

    // Start the file write and the thumbnail image first; they run while
    // the clipboard is written.
    let saved = settings.auto_save.then(|| {
        let (dir, png) = (settings.output_dir(), shot.png.clone());
        cx.background_executor()
            .spawn(async move { files::save_screenshot(&dir, taken_at, &png) })
    });
    let preview = settings.show_thumbnail.then(|| {
        let rgba = shot.rgba.clone();
        let (w, h) = thumbnail::image_size(width, height);
        let scale = monitor.scale_factor;
        let (max_w, max_h) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
        cx.background_executor()
            .spawn(async move { thumbnail::scale_down(rgba, width, height, max_w, max_h) })
    });
    // Without auto-save, the thumbnail writes a temporary file only if it
    // is opened or dragged.
    let unsaved = (settings.show_thumbnail && !settings.auto_save).then(|| shot.png.clone());
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

    let saved_path = saved.as_ref().and_then(|r| r.as_ref().ok().cloned());
    let result = match (copied, saved) {
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
    };
    result?;

    if settings.notify_after_capture {
        let message = match &saved_path {
            Some(path) => format!(
                "Saved as {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
            None => "On the clipboard".to_string(),
        };
        state
            .platform
            .notify(format!("Screenshot {width} × {height}"), message);
    }
    if let Some(preview) = preview {
        let file = match saved_path {
            Some(path) => CaptureFile::Saved(path),
            None => CaptureFile::Unsaved {
                png: unsaved.unwrap_or_default(),
                taken_at,
            },
        };
        let thumbnail = PendingThumbnail {
            small: preview.await,
            size: (width, height),
            file,
            monitor: monitor.clone(),
            seconds: settings.thumbnail_seconds,
        };
        show_thumbnail(state.clone(), thumbnail, cx);
    }
    Ok(())
}

/// The file behind a thumbnail: saved already, or written to the temporary
/// folder the first time it is opened or dragged.
enum CaptureFile {
    Saved(PathBuf),
    Unsaved {
        png: Vec<u8>,
        taken_at: NaiveDateTime,
    },
}

impl CaptureFile {
    async fn path(&mut self, cx: &mut AsyncApp) -> Result<PathBuf, String> {
        match self {
            CaptureFile::Saved(path) => Ok(path.clone()),
            CaptureFile::Unsaved { png, taken_at } => {
                let (png, taken_at) = (std::mem::take(png), *taken_at);
                let path = cx
                    .background_executor()
                    .spawn(
                        async move { files::save_screenshot(&files::temp_dir(), taken_at, &png) },
                    )
                    .await
                    .map_err(|e| format!("Could not write the screenshot: {e:#}"))?;
                *self = CaptureFile::Saved(path.clone());
                Ok(path)
            }
        }
    }
}

/// A thumbnail about to be shown.
struct PendingThumbnail {
    /// The screenshot scaled down: the card's picture and the drag image.
    small: thumbnail::Small,
    /// The screenshot's size, physical pixels.
    size: (u32, u32),
    file: CaptureFile,
    monitor: MonitorInfo,
    seconds: u32,
}

/// Where the thumbnail card goes: the bottom-right corner of the monitor's
/// work area (above the taskbar), physical pixels.
pub fn thumbnail_rect(work: PhysicalRect, scale: f32, size: (u32, u32)) -> PhysicalRect {
    let (w, h) = thumbnail::image_size(size.0, size.1);
    let card = |side: f32| ((side + 2.0 * thumbnail::PADDING) * scale).round() as u32;
    let (width, height) = (card(w), card(h));
    let margin = (THUMBNAIL_MARGIN * scale).round() as i32;
    PhysicalRect::new(
        work.x + work.width as i32 - width as i32 - margin,
        work.y + work.height as i32 - height as i32 - margin,
        width,
        height,
    )
}

/// Show the thumbnail, replacing any previous one, and handle it until it
/// closes. Runs on its own: the next capture does not wait for it.
fn show_thumbnail(state: Rc<State>, pending: PendingThumbnail, cx: &mut AsyncApp) {
    cx.spawn(async move |cx| {
        let PendingThumbnail {
            small,
            size,
            mut file,
            monitor,
            seconds,
        } = pending;
        let work = platform_window::work_area(monitor.id.0)
            .map(|(x, y, w, h)| PhysicalRect::new(x, y, w, h))
            .unwrap_or(monitor.bounds);
        let rect = thumbnail_rect(work, monitor.scale_factor, size);
        let image = thumbnail::render_image(&small);
        let drag_picture = thumbnail::soften(&small, thumbnail::DRAG_LOOK);
        let over = move || {
            platform_window::cursor_position().is_some_and(|(x, y)| {
                x >= rect.x
                    && y >= rect.y
                    && x < rect.x + rect.width as i32
                    && y < rect.y + rect.height as i32
            })
        };
        let opened = popup::open(&monitor, rect, false, cx, move |_, cx| {
            cx.new(|cx| Thumbnail::new(image, seconds, cx).with_pointer_probe(Box::new(over)))
        });
        let (card, mut events) = match opened {
            Ok(opened) => opened,
            Err(e) => {
                log::warn!("could not show the thumbnail: {e}");
                return;
            }
        };
        if let Some(hwnd) = card.hwnd() {
            platform_window::round_corners(hwnd);
            // Drags start here; it is never where they end.
            if let Err(e) = framecut_platform::drag::refuse_drops(hwnd) {
                log::warn!("the thumbnail still accepts drops: {e:#}");
            }
            if std::env::var_os("FRAMECUT_CAPTURABLE_UI").is_none()
                && let Err(e) = platform_window::exclude_from_capture(hwnd)
            {
                log::warn!("could not exclude the thumbnail from capture: {e:#}");
            }
        }
        let generation = state.thumbnail_generation.get() + 1;
        state.thumbnail_generation.set(generation);
        if let Some(previous) = state.thumbnail.replace(Some(card)) {
            previous.close(cx);
        }

        while let Some(event) = events.next().await {
            match event {
                ThumbnailEvent::Open => {
                    match file.path(cx).await {
                        Ok(path) => {
                            log::info!("thumbnail: opening the screenshot");
                            cx.update(|cx| cx.open_with_system(&path));
                        }
                        Err(e) => log::error!("{e}"),
                    }
                    break;
                }
                ThumbnailEvent::Drag => match file.path(cx).await {
                    // A modal loop until the drop; this task is outside any
                    // GPUI update, so the windows keep working meanwhile.
                    Ok(path) => match framecut_platform::drag::drag_file(
                        &path,
                        Some(DragImage {
                            width: drag_picture.width,
                            height: drag_picture.height,
                            rgba: &drag_picture.rgba,
                        }),
                    ) {
                        Ok(true) => {
                            log::info!("thumbnail: dropped into another application");
                            break;
                        }
                        Ok(false) => log::info!("thumbnail: drag cancelled"),
                        Err(e) => log::warn!("thumbnail drag failed: {e:#}"),
                    },
                    Err(e) => log::error!("{e}"),
                },
                ThumbnailEvent::Close => break,
            }
        }
        // Close it unless a newer thumbnail has replaced it.
        if state.thumbnail_generation.get() == generation
            && let Some(card) = state.thumbnail.take()
        {
            card.close(cx);
        }
    })
    .detach();
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
        assert!(menu.contains(&MenuItem::item(MENU_CAPTURE_BAR, "Capture Bar\tCtrl+Alt+C")));
        assert!(menu.contains(&MenuItem::item(
            MENU_SCREENSHOT,
            "Screenshot an area\tCtrl+Alt+S"
        )));
        let off = Settings {
            auto_save: false,
            ..settings
        };
        assert!(!auto_save_checked(&tray_menu(&off)));
    }

    #[test]
    fn the_thumbnail_sits_above_the_taskbar_at_the_right() {
        // 150%, work area 3840×2088 (a 72-pixel taskbar), a 16:9 screenshot:
        // a 240×135 image plus 6 pixels of padding, 16 from the edges.
        let work = PhysicalRect::new(0, 0, 3840, 2088);
        let rect = thumbnail_rect(work, 1.5, (3840, 2160));
        assert_eq!((rect.width, rect.height), (378, 221));
        assert_eq!((rect.x, rect.y), (3840 - 378 - 24, 2088 - 221 - 24));
    }

    #[test]
    fn the_capture_bar_sits_centred_near_the_top() {
        let monitor = MonitorInfo {
            id: MonitorId(1),
            device_name: String::new(),
            name: String::new(),
            bounds: PhysicalRect::new(3840, 0, 3840, 2160),
            scale_factor: 1.5,
            advanced_color_enabled: true,
            hdr_enabled: true,
            sdr_white_level_nits: Some(240.0),
        };
        let rect = bar_rect(&monitor);
        assert_eq!((rect.width, rect.height), (468, 198));
        assert_eq!((rect.x, rect.y), (3840 + (3840 - 468) / 2, 36));
    }
}
