//! The screenshot flows (PRD §7.2–7.5). The Capture Bar hotkey opens the
//! Capture Bar, which asks for Area, Window, Display or Freeform; the main
//! window asks the same, and clicking the tray icon opens that window. The
//! screenshot hotkey goes straight to Area. Area, Window and Freeform freeze
//! every monitor and show the overlay on each, the one under the pointer
//! first; Display captures the monitor under the pointer at once. The PNG
//! goes to the clipboard and, with auto-save on, the output folder.
//!
//! Recording (PRD §7.7) starts from the Capture Bar's Record mode or the
//! recording hotkey: the same overlay chooses the area, then the recorder
//! runs on its own thread until the hotkey is pressed again, and the MP4
//! goes to the recordings folder.
//!
//! Capture, clipboard and file work happen on the capture thread, the
//! platform thread and GPUI's background executor; this code only awaits
//! them, so the GPUI main thread never blocks (§17).
//!
//! This module holds the state the running app shares and the loop that
//! turns platform events into requests. The rest is split by
//! responsibility:
//!
//! - `tray`: the hotkey and menu ids, which hotkeys apply when, and the
//!   tray menu.
//! - `windows`: the main window, with its Settings page and the screenshot
//!   it shows.
//! - `screenshot`: the Capture Bar, freezing the monitors, and the
//!   selection overlay.
//! - `recording`: recording, its countdown, border and controls, and
//!   Restart, Discard and Undo.
//! - `delivery`: copying and saving a screenshot, then showing it in the
//!   window or as a thumbnail.
//! - `failure`: what a failure tells the user and the log.

use shuttercrab_types::WindowId;
mod delivery;
mod failure;
mod recording;
mod screenshot;
mod tray;
mod windows;

use delivery::show_file_in_main;
pub use delivery::thumbnail_rect;
pub use failure::Failure;
pub use screenshot::bar_rect;
pub use tray::{
    CAPTURE_BAR_HOTKEY, DISCARD_HOTKEY, MENU_AUTO_SAVE, MENU_CAPTURE_BAR, MENU_OPEN,
    MENU_OPEN_FOLDER, MENU_PAUSE, MENU_QUIT, MENU_RECORD, MENU_SCREENSHOT, MENU_SETTINGS,
    MENU_UPDATE, PAUSE_HOTKEY, QUIT_HOTKEY, QUIT_KEYS, RECORD_HOTKEY, RESTART_HOTKEY,
    SCREENSHOT_HOTKEY, SCREENSHOT_WITH_WINDOW_HOTKEY, TrayRecording, UNDO_HOTKEY, hotkeys,
    tray_menu,
};

use crate::{
    capture_choice::CaptureTarget,
    heap,
    main_window::{MainWindow, Page},
    popup,
    record_bar::Destructive,
    settings::{self, Settings},
    settings_window::OnPrintScreen,
    update::{self, UpdateBackend},
};
use futures::{StreamExt as _, channel::mpsc::UnboundedReceiver};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, AsyncApp, BackgroundExecutor, CursorHideMode, Entity,
    QuitMode,
};
use recording::{
    Followed, PreviousTake, Recording, displays_changed, finish_recording, keep_previous_take,
    record, report_recording, request, stop_recording, toggle_pause, undo, window_changed,
};
use screenshot::{capture, capture_bar};
use shuttercrab_capture::{Capture, monitor_under_pointer};
use shuttercrab_platform::{Hotkey, Platform, PlatformEvent, Request, window as platform_window};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use tray::{TRAY_DELAY, print_screen_hotkey};
use windows::{open_folder, open_main};

/// Everything the running app shares between its tasks.
pub struct Shuttercrab {
    pub capture: Capture,
    pub platform: Platform,
    pub settings: Settings,
    /// Where settings are saved; `None` if there is no config folder.
    pub settings_path: Option<PathBuf>,
    /// Where the log is written, for the Diagnostics page.
    pub log_dir: Option<PathBuf>,
    /// Updates from GitHub Releases; `None` when not installed by Velopack.
    pub updates: Option<Arc<dyn UpdateBackend>>,
    /// What to do at once: show the window (a plain start), what a
    /// command line asked for, or nothing (a start at sign-in).
    pub start: Option<Request>,
}

struct State {
    capture: Capture,
    platform: Platform,
    updates: Option<Arc<dyn UpdateBackend>>,
    /// A downloaded release's version, waiting for "Restart to update".
    update_ready: RefCell<Option<String>>,
    settings: Rc<RefCell<Settings>>,
    log_dir: Option<PathBuf>,
    /// The main window, while it is open.
    main_window: RefCell<Option<(AnyWindowHandle, Entity<MainWindow>)>>,
    /// The main window, hidden while a capture it started is chosen and
    /// taken (or recorded); shown again when that is over.
    hidden_main: Cell<Option<WindowId>>,
    /// What clicking the latest notification opens, if anything.
    notified: RefCell<Option<PathBuf>>,
    settings_path: Option<PathBuf>,
    /// One capture at a time: a second request while the Capture Bar or
    /// the overlay is up is ignored; one while a screenshot is being copied
    /// and saved waits for it, in `next`.
    busy: Cell<Busy>,
    next: Cell<Option<(Start, Option<Duration>)>>,
    /// The thumbnail on screen, and a count that tells its handler whether
    /// a newer one has replaced it.
    thumbnail: RefCell<Option<popup::Popup>>,
    thumbnail_generation: Cell<u64>,
    /// The recording in progress, if any.
    recording: RefCell<Option<Recording>>,
    /// The recorded window, followed from when it is chosen until its
    /// recording ends.
    followed: RefCell<Option<Followed>>,
    /// The take a restart replaced, kept until its undo window closes.
    previous_take: RefCell<Option<PreviousTake>>,
    /// Counts undo windows, so a timer knows whether its window is still
    /// the current one.
    undo_generation: Cell<u64>,
    /// While a hotkey field records: what it does with Print Screen.
    print_screen: RefCell<Option<OnPrintScreen>>,
}

impl State {
    /// The recording, as the tray menu shows it.
    fn tray_recording(&self) -> TrayRecording {
        match &*self.recording.borrow() {
            None => TrayRecording::Idle,
            Some(r) if r.clock.get().is_paused() => TrayRecording::Paused,
            Some(_) => TrayRecording::Running,
        }
    }

    /// Show the tray menu for the current settings and recording state.
    fn refresh_tray_menu(&self) {
        let menu = tray_menu(
            &self.settings.borrow(),
            self.tray_recording(),
            self.update_ready.borrow().as_deref(),
        );
        self.platform.set_tray_menu(menu);
    }

    /// Hide the main window while a capture is chosen and taken, as
    /// Snipping Tool hides its own, if it is on screen. Left out of captures
    /// at once, so the capture need not wait for it to fade away.
    fn hide_main(&self, cx: &mut AsyncApp) {
        if self.hidden_main.get().is_some() {
            return;
        }
        let open = self.main_window.borrow().clone();
        let os_window = open.and_then(|(window, _)| {
            window
                .update(cx, |_, window, _| popup::os_window(window))
                .ok()
                .flatten()
        });
        if let Some(os_window) =
            os_window.filter(|os_window| platform_window::is_on_screen(*os_window))
        {
            if let Err(e) = platform_window::exclude_from_capture(os_window) {
                log::warn!("the main window may show in the capture: {e:#}");
            }
            platform_window::hide(os_window);
            self.hidden_main.set(Some(os_window));
        }
    }

    /// Show the main window again if a capture hid it, once nothing is
    /// being chosen or recorded, with the keyboard.
    fn show_hidden_main(&self) {
        if self.busy.get() == Busy::Idle
            && self.recording.borrow().is_none()
            && let Some(os_window) = self.hidden_main.take()
        {
            platform_window::include_in_capture(os_window);
            platform_window::show_normal(os_window);
        }
    }

    /// A recording started, ended, or opened or closed an undo window:
    /// update the tray menu, and register the hotkeys that apply now.
    fn recording_changed(&self, cx: &AsyncApp) {
        self.refresh_tray_menu();
        self.show_hidden_main();
        let registering = self.platform.set_hotkeys(self.hotkeys());
        // Nothing waits for the answer; a taken hotkey is only logged.
        cx.background_executor()
            .spawn(async move {
                for conflict in registering.await {
                    log::warn!("{} is taken by another application", conflict.hotkey);
                }
            })
            .detach();
    }

    /// Change the settings and save them in the background.
    fn update_settings(&self, change: impl FnOnce(&mut Settings), cx: &mut AsyncApp) -> Settings {
        let settings = {
            let mut settings = self.settings.borrow_mut();
            change(&mut settings);
            settings.clone()
        };
        self.save(&settings, cx.background_executor());
        settings
    }

    /// Show a notification; clicking it opens `opens`, if given.
    fn notify(&self, title: impl Into<String>, message: impl Into<String>, opens: Option<PathBuf>) {
        self.notified.replace(opens);
        self.platform.notify(title, message);
    }

    /// Save `settings` in the background.
    fn save(&self, settings: &Settings, executor: &BackgroundExecutor) {
        if let Some(path) = self.settings_path.clone() {
            let saved = settings.clone();
            executor
                .spawn(async move {
                    if let Err(e) = settings::save(&path, &saved) {
                        log::error!("could not save settings: {e:#}");
                    }
                })
                .detach();
        }
    }

    /// The global hotkeys the settings name, for now.
    fn hotkeys(&self) -> Vec<(u32, Hotkey)> {
        let recording = self.recording.borrow();
        let undo = self.previous_take.borrow().is_some()
            || recording.as_ref().is_some_and(|r| r.discarded.is_some());
        hotkeys(&self.settings.borrow(), recording.is_some(), undo)
    }

    /// A new undo window's number.
    fn next_generation(&self) -> u64 {
        let generation = self.undo_generation.get() + 1;
        self.undo_generation.set(generation);
        generation
    }
}

/// What a request starts with.
#[derive(Clone, Copy, Debug)]
enum Start {
    CaptureBar,
    Screenshot(CaptureTarget),
    /// An area screenshot that leaves Shuttercrab's window in the picture.
    ScreenshotWithWindow,
    Record(CaptureTarget),
}

/// What the current request is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Busy {
    Idle,
    /// The Capture Bar, the overlay or a countdown is up: the user is busy
    /// with it.
    Choosing,
    /// A screenshot is being copied and saved, for a few milliseconds
    /// (longer while another app holds the clipboard).
    Finishing,
}

/// Start handling platform events. Call once, inside the GPUI application.
pub fn run(shuttercrab: Shuttercrab, events: UnboundedReceiver<PlatformEvent>, cx: &mut App) {
    // Shuttercrab keeps running in the tray when its window closes. GPUI's
    // default on Windows quits when the last window closes, which would end
    // the app then, or the moment a selection finishes, before the
    // clipboard is written.
    cx.set_quit_mode(QuitMode::Explicit);
    // GPUI hides the pointer while typing; Shuttercrab's keys type nothing,
    // and the pointer should stay where the user drags.
    cx.set_cursor_hide_mode(CursorHideMode::Never);
    let state = Rc::new(State {
        capture: shuttercrab.capture,
        platform: shuttercrab.platform,
        updates: shuttercrab.updates,
        update_ready: RefCell::new(None),
        settings: Rc::new(RefCell::new(shuttercrab.settings)),
        settings_path: shuttercrab.settings_path,
        log_dir: shuttercrab.log_dir,
        main_window: RefCell::new(None),
        hidden_main: Cell::new(None),
        notified: RefCell::new(None),
        busy: Cell::new(Busy::Idle),
        next: Cell::new(None),
        thumbnail: RefCell::new(None),
        thumbnail_generation: Cell::new(0),
        recording: RefCell::new(None),
        followed: RefCell::new(None),
        previous_take: RefCell::new(None),
        undo_generation: Cell::new(0),
        print_screen: RefCell::new(None),
    });
    watch_for_updates(&state, cx);
    let first = shuttercrab.start;
    cx.spawn(async move |cx| {
        // Shuttercrab's window, on the taskbar like any app's, until closed;
        // or what the command line asked for.
        if let Some(request) = first {
            requested(&state, request, cx);
        }
        heap::log_memory_soon("idle after start", cx);
        let mut events = events;
        while let Some(event) = events.next().await {
            match event {
                PlatformEvent::Hotkey(SCREENSHOT_HOTKEY) => {
                    start(&state, Start::Screenshot(CaptureTarget::Area), None, cx)
                }
                PlatformEvent::Hotkey(SCREENSHOT_WITH_WINDOW_HOTKEY) => {
                    start(&state, Start::ScreenshotWithWindow, None, cx)
                }
                PlatformEvent::Hotkey(CAPTURE_BAR_HOTKEY)
                | PlatformEvent::TrayCommand(MENU_CAPTURE_BAR) => {
                    start(&state, Start::CaptureBar, None, cx)
                }
                PlatformEvent::TrayCommand(MENU_SCREENSHOT) => start(
                    &state,
                    Start::Screenshot(CaptureTarget::Area),
                    Some(TRAY_DELAY),
                    cx,
                ),
                PlatformEvent::Hotkey(PAUSE_HOTKEY) | PlatformEvent::TrayCommand(MENU_PAUSE) => {
                    toggle_pause(&state, cx)
                }
                PlatformEvent::Hotkey(RESTART_HOTKEY) => {
                    request(&state, Destructive::Restart, cx).await
                }
                PlatformEvent::Hotkey(DISCARD_HOTKEY) => {
                    request(&state, Destructive::Discard, cx).await
                }
                PlatformEvent::Hotkey(UNDO_HOTKEY) => undo(&state, cx),
                PlatformEvent::Hotkey(RECORD_HOTKEY) => toggle_recording(&state, None, cx),
                PlatformEvent::TrayCommand(MENU_RECORD) => {
                    toggle_recording(&state, Some(TRAY_DELAY), cx)
                }
                PlatformEvent::TrayCommand(MENU_OPEN_FOLDER) => open_folder(&state, cx),
                // Clicking the tray icon opens the window, as in any tray app.
                PlatformEvent::TrayActivated | PlatformEvent::TrayCommand(MENU_OPEN) => {
                    open_main(&state, Page::Home, cx)
                }
                PlatformEvent::TrayCommand(MENU_SETTINGS) => open_main(&state, Page::Settings, cx),
                PlatformEvent::TrayCommand(MENU_AUTO_SAVE) => {
                    let settings = state.update_settings(|s| s.auto_save = !s.auto_save, cx);
                    log::info!(
                        "auto-save {}",
                        if settings.auto_save { "on" } else { "off" }
                    );
                    state.refresh_tray_menu();
                }
                PlatformEvent::Hotkey(QUIT_HOTKEY) | PlatformEvent::TrayCommand(MENU_QUIT) => {
                    quit(&state, cx).await
                }
                PlatformEvent::TrayCommand(MENU_UPDATE) => restart_to_update(&state, cx).await,
                PlatformEvent::AnotherInstance(request) => {
                    log::info!("Shuttercrab was started again, for {}", request.name());
                    requested(&state, request, cx);
                }
                PlatformEvent::NotificationClicked => notification_clicked(&state, cx),
                PlatformEvent::DisplaysChanged => displays_changed(&state),
                PlatformEvent::Window(change) => window_changed(&state, change, cx),
                PlatformEvent::Hotkey(id) => print_screen_pressed(&state, id, cx),
                PlatformEvent::TrayCommand(_) => {}
            }
        }
    })
    .detach();
}

/// Print Screen, pressed while a hotkey field records: tell the field.
fn print_screen_pressed(state: &Rc<State>, id: u32, cx: &mut AsyncApp) {
    let listener = state.print_screen.borrow().clone();
    if let (Some(hotkey), Some(listener)) = (print_screen_hotkey(id), listener) {
        cx.update(|cx| listener(hotkey, cx));
    }
}

/// Open what the latest notification is about, like clicking the
/// thumbnail: a screenshot shows in the window; a recording opens in the
/// system's player.
fn notification_clicked(state: &Rc<State>, cx: &mut AsyncApp) {
    let opens = state.notified.borrow().clone();
    match opens {
        Some(path) if path.extension().is_some_and(|e| e == "png") => {
            let state = state.clone();
            cx.spawn(async move |cx| show_file_in_main(&state, path, cx).await)
                .detach();
        }
        Some(path) => cx.update(|cx| cx.open_with_system(&path)),
        None => {}
    }
}

/// Look for a newer release now and every few hours, in the background,
/// until one is downloaded; then offer it in the tray menu and the main
/// window.
fn watch_for_updates(state: &Rc<State>, cx: &mut App) {
    let Some(backend) = state.updates.clone() else {
        return;
    };
    let state = state.clone();
    cx.spawn(async move |cx| {
        loop {
            match cx.background_spawn(backend.fetch()).await {
                Ok(Some(version)) => {
                    log::info!("Shuttercrab {version} is downloaded and ready");
                    state.update_ready.replace(Some(version.clone()));
                    state.refresh_tray_menu();
                    let main = state.main_window.borrow().clone();
                    if let Some((_, view)) = main {
                        view.update(cx, |_, cx| cx.notify());
                    }
                    // A notification would show in a recording of the
                    // screen; the tray menu offers the update when it ends.
                    if state.tray_recording() == TrayRecording::Idle {
                        state.notify(
                            format!("Shuttercrab {version} is ready"),
                            "Restart to update from the tray menu or the Shuttercrab window.",
                            None,
                        );
                    }
                    return;
                }
                Ok(None) => log::debug!("no newer release"),
                // Offline or rate limited: the next check tries again.
                Err(e) => log::info!("could not check for updates: {e:#}"),
            }
            cx.background_executor().timer(update::CHECK_EVERY).await;
        }
    })
    .detach();
}

/// Quit. Nothing is lost: a recording running at quit is finished as if
/// stopped (even one discarded moments ago), and a take a restart replaced
/// is kept.
async fn quit(state: &Rc<State>, cx: &mut AsyncApp) {
    let previous = state.previous_take.take();
    if let Some(previous) = previous {
        keep_previous_take(previous, cx).await;
    }
    let recording = state.recording.take();
    if let Some(Recording {
        recorder: Some(recorder),
        path,
        ..
    }) = recording
    {
        log::info!("finishing the recording before quitting");
        let finished = finish_recording(recorder, path, cx).await;
        report_recording(state, finished);
    }
    log::info!("quitting");
    cx.update(|cx| cx.quit());
}

/// Quit so the updater can apply the downloaded release and start it.
/// Never while a capture is open or a recording runs: the menu hides the
/// offer then, and a late click is ignored.
async fn restart_to_update(state: &Rc<State>, cx: &mut AsyncApp) {
    let Some(backend) = state.updates.clone() else {
        return;
    };
    if state.busy.get() != Busy::Idle || state.recording.borrow().is_some() {
        log::info!("not restarting to update: a capture or recording is in progress");
        return;
    }
    match backend.apply_after_exit() {
        Ok(()) => {
            log::info!("restarting to update");
            quit(state, cx).await;
        }
        Err(e) => {
            log::error!("could not start the update: {e:#}");
            state.notify(
                "Could not update",
                "Shuttercrab will update the next time it starts.",
                None,
            );
        }
    }
}

/// The command a desktop shortcut runs for `request`: this program, where
/// it was started from (the AppImage, when it is one), and the request's
/// flag. For a system without global hotkeys.
pub fn shortcut_command(request: Request) -> String {
    let program = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .map_or_else(|| "shuttercrab".to_string(), |p| p.display().to_string());
    let program = if program.contains(' ') {
        format!("\"{program}\"")
    } else {
        program
    };
    format!("{program} --{}", request.name())
}

/// Do what a start of Shuttercrab asked for: a plain start shows the
/// window, as starting any running app does; the others act as their
/// hotkey would.
fn requested(state: &Rc<State>, request: Request, cx: &mut AsyncApp) {
    match request {
        Request::Show => open_main(state, Page::Home, cx),
        Request::CaptureBar => start(state, Start::CaptureBar, None, cx),
        Request::Screenshot => start(state, Start::Screenshot(CaptureTarget::Area), None, cx),
        Request::Record => toggle_recording(state, None, cx),
    }
}

/// One hotkey starts a recording and stops it. `delay` lets a menu that
/// asked close first.
fn toggle_recording(state: &Rc<State>, delay: Option<Duration>, cx: &mut AsyncApp) {
    if state.recording.borrow().is_some() {
        stop_recording(state, cx);
    } else {
        start(state, Start::Record(CaptureTarget::Area), delay, cx);
    }
}

fn start(state: &Rc<State>, what: Start, delay: Option<Duration>, cx: &mut AsyncApp) {
    match state.busy.get() {
        Busy::Idle => {}
        Busy::Choosing => {
            log::info!("{what:?} ignored: a capture is already open");
            return;
        }
        Busy::Finishing => {
            log::info!("{what:?} waits for the screenshot being finished");
            state.next.set(Some((what, delay)));
            return;
        }
    }
    state.busy.set(Busy::Choosing);
    if !matches!(what, Start::ScreenshotWithWindow) {
        state.hide_main(cx);
    }
    let state = state.clone();
    cx.spawn(async move |cx| {
        if let Some(delay) = delay {
            cx.background_executor().timer(delay).await;
        }
        let pressed = Instant::now();
        let result = match monitor_under_pointer() {
            None => Err(Failure::plain("No monitor is under the pointer.")),
            Some(monitor) => match what {
                Start::CaptureBar => capture_bar(&state, monitor, pressed, cx).await,
                Start::Screenshot(target) => capture(&state, target, monitor, pressed, cx).await,
                Start::ScreenshotWithWindow => {
                    capture(&state, CaptureTarget::Area, monitor, pressed, cx).await
                }
                Start::Record(target) => record(&state, target, monitor, pressed, cx).await,
            },
        };
        if let Err(failure) = result {
            log::error!("{}", failure.detail);
            state.notify(failure.title(), failure.message, None);
        }
        state.busy.set(Busy::Idle);
        match state.next.take() {
            Some((what, delay)) => start(&state, what, delay, cx),
            // In a task of its own, so the window is shown after it has
            // been sized for the screenshot, which waits for its own task.
            None => cx.spawn(async move |_| state.show_hidden_main()).detach(),
        }
        heap::log_memory_soon("after a capture request", cx);
    })
    .detach();
}
