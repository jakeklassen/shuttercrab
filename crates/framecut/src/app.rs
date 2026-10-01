//! The screenshot flows (PRD §7.2–7.5). The Capture Bar hotkey or the tray
//! icon opens the Capture Bar, which asks for Area, Window or Display; the
//! screenshot hotkey goes straight to Area. Area and Window freeze the
//! monitor under the pointer and show the overlay; Display captures the
//! monitor at once. The PNG goes to the clipboard and, with auto-save on,
//! the output folder.
//!
//! Recording (PRD §7.7) starts from the Capture Bar's Record mode or the
//! recording hotkey: the same overlay chooses the area, then the recorder
//! runs on its own thread until the hotkey is pressed again, and the MP4
//! goes to the recordings folder.
//!
//! Capture, clipboard and file work happen on the capture thread, the
//! platform thread and GPUI's background executor; this code only awaits
//! them, so the GPUI main thread never blocks (§17).

use crate::{
    capture_bar::{BAR_HEIGHT, BAR_WIDTH, CaptureBar, CaptureBarEvent, CaptureMode, CaptureTarget},
    countdown::{COUNTDOWN_HEIGHT, COUNTDOWN_WIDTH, Countdown, CountdownEvent},
    files,
    overlay::{Mode, OverlayEvent, OverlayFrame, SelectionOverlay},
    popup::{self, Activation},
    record_bar::{
        BarMode, Destructive, RECORD_BAR_HEIGHT, RECORD_BAR_WIDTH, RecordBar, RecordBarEvent,
        RecordKeys,
    },
    recording::{self, Clock},
    selection::ScreenWindow,
    settings::{self, Settings},
    settings_window::{Diagnostics, Hooks, SettingsWindow},
    thumbnail::{self, Thumbnail, ThumbnailEvent},
};
use chrono::{Local, NaiveDateTime};
use framecut_capture::{
    Capture, FrozenFrame, MonitorId, MonitorInfo, PhysicalRect, Screenshot, monitor_under_pointer,
    record::{RecordOptions, Recorder, RecordingSummary},
};
use framecut_platform::{
    Hotkey, MenuItem, Platform, PlatformEvent,
    drag::DragImage,
    frame::{Frame, FrameStyle, Rect as FrameRect},
    targets, window as platform_window,
};
use futures::{StreamExt as _, channel::mpsc::UnboundedReceiver};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, AsyncApp, BackgroundExecutor, Bounds, Entity, QuitMode,
    TitlebarOptions, WindowBounds, WindowKind, WindowOptions,
    component::{Root, Theme},
    px, size,
};
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
pub const RECORD_HOTKEY: u32 = 4;
pub const PAUSE_HOTKEY: u32 = 5;
pub const RESTART_HOTKEY: u32 = 6;
pub const DISCARD_HOTKEY: u32 = 7;
pub const UNDO_HOTKEY: u32 = 8;

/// Ids for the tray menu's items.
pub const MENU_SCREENSHOT: u32 = 1;
pub const MENU_OPEN_FOLDER: u32 = 2;
pub const MENU_AUTO_SAVE: u32 = 3;
pub const MENU_QUIT: u32 = 4;
pub const MENU_CAPTURE_BAR: u32 = 5;
pub const MENU_SETTINGS: u32 = 6;
pub const MENU_RECORD: u32 = 7;
pub const MENU_PAUSE: u32 = 8;

/// Quits without the tray menu; kept for development.
pub const QUIT_KEYS: &str = "Ctrl+Alt+Shift+Q";

/// The global hotkeys for `settings`: the Capture Bar, area screenshots,
/// recording, and quit; pause, restart and discard while `recording`; and
/// undo while there is something to `undo`. Other apps keep those chords
/// the rest of the time. A hotkey the settings spell wrongly is left out
/// (main checks them at startup; the settings window only stores valid
/// ones).
pub fn hotkeys(settings: &Settings, recording: bool, undo: bool) -> Vec<(u32, Hotkey)> {
    let while_recording = [
        (PAUSE_HOTKEY, settings.pause_hotkey.as_str()),
        (RESTART_HOTKEY, settings.restart_hotkey.as_str()),
        (DISCARD_HOTKEY, settings.discard_hotkey.as_str()),
    ];
    let undoing = [(UNDO_HOTKEY, settings.undo_hotkey.as_str())];
    [
        (CAPTURE_BAR_HOTKEY, settings.capture_bar_hotkey.as_str()),
        (SCREENSHOT_HOTKEY, settings.screenshot_hotkey.as_str()),
        (RECORD_HOTKEY, settings.record_hotkey.as_str()),
        (QUIT_HOTKEY, QUIT_KEYS),
    ]
    .into_iter()
    .chain(while_recording.into_iter().filter(|_| recording))
    .chain(undoing.into_iter().filter(|_| undo))
    .filter_map(|(id, text)| Some((id, Hotkey::parse(text).ok()?)))
    .collect()
}

/// A recording, as the tray menu shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayRecording {
    Idle,
    Running,
    Paused,
}

/// A capture started from the tray menu waits this long, so the menu has
/// closed before the screen is frozen.
const TRAY_DELAY: Duration = Duration::from_millis(250);

/// The Capture Bar's distance from the top of the monitor, logical pixels.
const BAR_TOP: f32 = 24.0;

/// The thumbnail's distance from the work area's edges, logical pixels.
const THUMBNAIL_MARGIN: f32 = 16.0;

/// The tray icon's context menu for the current settings and recording.
pub fn tray_menu(settings: &Settings, recording: TrayRecording) -> Vec<MenuItem> {
    let record = match recording {
        TrayRecording::Idle => "Record an area",
        TrayRecording::Running | TrayRecording::Paused => "Stop recording",
    };
    let pause = match recording {
        TrayRecording::Idle => None,
        TrayRecording::Running => Some("Pause recording"),
        TrayRecording::Paused => Some("Resume recording"),
    };
    let pause = pause
        .map(|label| MenuItem::item(MENU_PAUSE, format!("{label}\t{}", settings.pause_hotkey)));
    [
        // A tab right-aligns the rest of the label, like a menu accelerator.
        MenuItem::item(
            MENU_CAPTURE_BAR,
            format!("Capture Bar\t{}", settings.capture_bar_hotkey),
        ),
        MenuItem::item(
            MENU_SCREENSHOT,
            format!("Screenshot an area\t{}", settings.screenshot_hotkey),
        ),
        MenuItem::item(MENU_RECORD, format!("{record}\t{}", settings.record_hotkey)),
    ]
    .into_iter()
    .chain(pause)
    .chain(rest_of_tray_menu(settings))
    .collect()
}

/// The tray menu below the capture items.
fn rest_of_tray_menu(settings: &Settings) -> Vec<MenuItem> {
    vec![
        MenuItem::item(MENU_OPEN_FOLDER, "Open screenshots folder"),
        MenuItem::Separator,
        MenuItem::Item {
            id: MENU_AUTO_SAVE,
            label: "Save screenshots to the folder".into(),
            enabled: true,
            checked: settings.auto_save,
        },
        MenuItem::Separator,
        MenuItem::item(MENU_SETTINGS, "Settings…"),
        MenuItem::Separator,
        MenuItem::item(MENU_QUIT, "Quit Framecut"),
    ]
}

/// A failure to report: a message for the user, and the details for the log
/// (PRD §24).
#[derive(Debug)]
pub struct Failure {
    pub message: String,
    pub detail: String,
    /// Whether a recording failed, rather than a screenshot.
    pub recording: bool,
}

impl Failure {
    fn new(message: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            detail: detail.into(),
            recording: false,
        }
    }

    /// A failure whose message says it all.
    fn plain(message: &str) -> Self {
        Self::new(message, message)
    }

    /// The same failure, of a recording.
    fn of_recording(self) -> Self {
        Self {
            recording: true,
            ..self
        }
    }

    /// The notification's title.
    fn title(&self) -> &'static str {
        if self.recording {
            "Recording failed"
        } else {
            "Screenshot failed"
        }
    }
}

impl From<framecut_capture::CaptureError> for Failure {
    fn from(e: framecut_capture::CaptureError) -> Self {
        // The capture error's code and detail are for the log only.
        Self::new(e.message.clone(), format!("{:?}: {e}", e.code))
    }
}

/// Everything the running app shares between its tasks.
pub struct Framecut {
    pub capture: Capture,
    pub platform: Platform,
    pub settings: Settings,
    /// Where settings are saved; `None` if there is no config folder.
    pub settings_path: Option<PathBuf>,
    /// Where the log is written, for the Diagnostics page.
    pub log_dir: Option<PathBuf>,
}

struct State {
    capture: Capture,
    platform: Platform,
    settings: Rc<RefCell<Settings>>,
    log_dir: Option<PathBuf>,
    /// The settings window, while it is open.
    settings_window: RefCell<Option<AnyWindowHandle>>,
    /// What clicking the latest notification opens, if anything.
    notified: RefCell<Option<PathBuf>>,
    settings_path: Option<PathBuf>,
    /// One capture at a time: a second request while the Capture Bar or
    /// the overlay is up is ignored.
    busy: Cell<bool>,
    /// The thumbnail on screen, and a count that tells its handler whether
    /// a newer one has replaced it.
    thumbnail: RefCell<Option<popup::Popup>>,
    thumbnail_generation: Cell<u64>,
    /// The recording in progress, if any.
    recording: RefCell<Option<Recording>>,
    /// The take a restart replaced, kept until its undo window closes.
    previous_take: RefCell<Option<PreviousTake>>,
    /// Counts undo windows, so a timer knows whether its window is still
    /// the current one.
    undo_generation: Cell<u64>,
}

/// A finished take that a restart replaced: deleted when its undo window
/// closes, unless Undo keeps it.
struct PreviousTake {
    /// Its final name; until kept it stays at [`files::partial_path`].
    path: PathBuf,
    generation: u64,
}

/// The controls are asking before a take is thrown away.
#[derive(Clone, Copy, Debug)]
struct Asking {
    action: Destructive,
    /// The recording was running when asked, and resumes if kept.
    resume: bool,
    /// The window that had the keyboard before the bar took it to ask.
    give_back: Option<isize>,
}

/// A discard that can still be undone: the recording is paused meanwhile.
#[derive(Clone, Copy, Debug)]
struct Discarded {
    /// The recording was running, and resumes if the discard is undone.
    resume: bool,
    generation: u64,
    /// The window that had the keyboard before the controls took it.
    give_back: Option<isize>,
}

/// A recording in progress.
struct Recording {
    /// `None` only while Restart swaps in a new one.
    recorder: Option<Recorder>,
    /// Where the finished file goes; until then it is written to
    /// [`files::partial_path`] of it.
    path: PathBuf,
    /// What is recorded, for Restart.
    options: RecordOptions,
    /// The output length so far, shared with the controls.
    clock: Rc<Cell<Clock>>,
    /// The recording controls, if they are on screen.
    controls: Option<Controls>,
    /// A question the controls are asking, if any.
    asking: Option<Asking>,
    /// A discard that can still be undone, if any.
    discarded: Option<Discarded>,
    /// The dashed border around the recorded area, if it is shown.
    frame: Option<Frame>,
}

/// The border's colours (RGB): recording, paused, and discarded.
const FRAME_RECORDING: [u8; 3] = [0xE5, 0x48, 0x4D];
const FRAME_PAUSED: [u8; 3] = [0xF5, 0xA5, 0x24];
const FRAME_DISCARDED: [u8; 3] = [0x9D, 0x9D, 0x9D];
/// Before recording starts, during a countdown.
const FRAME_WAITING: [u8; 3] = FRAME_DISCARDED;

/// The border's look, logical pixels.
const FRAME_THICKNESS: f32 = 2.0;
const FRAME_DASH: f32 = 8.0;
const FRAME_GAP: f32 = 5.0;

/// The recording controls' window and view.
struct Controls {
    popup: popup::Popup,
    view: Entity<RecordBar>,
}

impl Recording {
    /// Redraw the controls after the clock changed.
    fn refresh_controls(&self, cx: &mut AsyncApp) {
        if let Some(controls) = &self.controls {
            controls.view.update(cx, |_, cx| cx.notify());
        }
    }

    /// Close the controls and remove the border.
    fn close_controls(&mut self, cx: &mut AsyncApp) {
        self.frame = None;
        if let Some(controls) = self.controls.take() {
            controls.popup.close(cx);
        }
    }

    /// The controls' view, to change what it shows.
    fn view(&self) -> Option<Entity<RecordBar>> {
        self.controls.as_ref().map(|c| c.view.clone())
    }

    /// Pause or resume the recorder and the clock. Returns whether that
    /// changed anything.
    fn set_paused(&self, paused: bool) -> bool {
        let Some(recorder) = &self.recorder else {
            return false;
        };
        let (mut clock, now) = (self.clock.get(), Instant::now());
        if clock.is_paused() == paused {
            return false;
        }
        if paused {
            recorder.pause();
            clock.pause(now);
        } else {
            recorder.resume();
            clock.resume(now);
        }
        self.clock.set(clock);
        self.color_frame_for_clock();
        true
    }

    /// Draw the border in `color`.
    fn color_frame(&self, color: [u8; 3]) {
        if let Some(frame) = &self.frame
            && let Err(e) = frame.recolor(color)
        {
            log::warn!("could not redraw the recording border: {e:#}");
        }
    }

    /// Draw the border red while recording, amber while paused.
    fn color_frame_for_clock(&self) {
        let paused = self.clock.get().is_paused();
        self.color_frame(if paused {
            FRAME_PAUSED
        } else {
            FRAME_RECORDING
        });
    }
}

/// Change what the controls show, outside any borrow of the state.
fn show(
    view: Option<Entity<RecordBar>>,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut RecordBar, &mut gpui_kit::Context<RecordBar>),
) {
    if let Some(view) = view {
        view.update(cx, f);
    }
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
        let menu = tray_menu(&self.settings.borrow(), self.tray_recording());
        self.platform.set_tray_menu(menu);
    }

    /// A recording started, ended, or opened or closed an undo window:
    /// update the tray menu, and register the hotkeys that apply now.
    fn recording_changed(&self, cx: &AsyncApp) {
        self.refresh_tray_menu();
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
    Record(CaptureTarget),
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
        settings: Rc::new(RefCell::new(framecut.settings)),
        settings_path: framecut.settings_path,
        log_dir: framecut.log_dir,
        settings_window: RefCell::new(None),
        notified: RefCell::new(None),
        busy: Cell::new(false),
        thumbnail: RefCell::new(None),
        thumbnail_generation: Cell::new(0),
        recording: RefCell::new(None),
        previous_take: RefCell::new(None),
        undo_generation: Cell::new(0),
    });
    cx.spawn(async move |cx| {
        let mut events = events;
        while let Some(event) = events.next().await {
            match event {
                PlatformEvent::Hotkey(SCREENSHOT_HOTKEY) => {
                    start(&state, Start::Screenshot(CaptureTarget::Area), None, cx)
                }
                PlatformEvent::Hotkey(CAPTURE_BAR_HOTKEY)
                | PlatformEvent::TrayActivated
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
                // One hotkey starts a recording and stops it.
                PlatformEvent::Hotkey(RECORD_HOTKEY) | PlatformEvent::TrayCommand(MENU_RECORD) => {
                    if state.recording.borrow().is_some() {
                        stop_recording(&state, cx);
                    } else {
                        let tray = matches!(event, PlatformEvent::TrayCommand(_));
                        let delay = tray.then_some(TRAY_DELAY);
                        start(&state, Start::Record(CaptureTarget::Area), delay, cx);
                    }
                }
                PlatformEvent::TrayCommand(MENU_OPEN_FOLDER) => open_folder(&state, cx),
                PlatformEvent::TrayCommand(MENU_SETTINGS) => open_settings(&state, cx),
                PlatformEvent::TrayCommand(MENU_AUTO_SAVE) => {
                    let settings = state.update_settings(|s| s.auto_save = !s.auto_save, cx);
                    log::info!(
                        "auto-save {}",
                        if settings.auto_save { "on" } else { "off" }
                    );
                    state.refresh_tray_menu();
                }
                PlatformEvent::Hotkey(QUIT_HOTKEY) | PlatformEvent::TrayCommand(MENU_QUIT) => {
                    // Nothing is lost by quitting: a recording running at
                    // quit is finished as if stopped (even one discarded
                    // moments ago), and a take a restart replaced is kept.
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
                        report_recording(&state, finished);
                    }
                    log::info!("quitting");
                    cx.update(|cx| cx.quit());
                }
                // Starting Framecut again (say, from the Start menu) shows its
                // settings: something visible, and the way to change it.
                PlatformEvent::AnotherInstance => {
                    log::info!("Framecut was started again; showing its settings");
                    open_settings(&state, cx);
                }
                // Like clicking the thumbnail: open the screenshot.
                PlatformEvent::NotificationClicked => {
                    let opens = state.notified.borrow().clone();
                    if let Some(path) = opens {
                        cx.update(|cx| cx.open_with_system(&path));
                    }
                }
                PlatformEvent::DisplaysChanged => displays_changed(&state),
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
            None => Err(Failure::plain("No monitor is under the pointer.")),
            Some(monitor) => match what {
                Start::CaptureBar => capture_bar(&state, monitor, pressed, cx).await,
                Start::Screenshot(target) => capture(&state, target, monitor, pressed, cx).await,
                Start::Record(target) => record(&state, target, monitor, pressed, cx).await,
            },
        };
        if let Err(failure) = result {
            log::error!("{}", failure.detail);
            state.notify(failure.title(), failure.message, None);
        }
        state.busy.set(false);
    })
    .detach();
}

fn open_folder(state: &State, cx: &mut AsyncApp) {
    let dir = state.settings.borrow().output_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::error!("could not create {}: {e}", dir.display());
        state.notify("Could not open the folder", e.to_string(), None);
        return;
    }
    cx.update(|cx| cx.open_with_system(&dir));
}

/// Open the settings window, or bring it forward if it is open.
fn open_settings(state: &Rc<State>, cx: &mut AsyncApp) {
    let open = *state.settings_window.borrow();
    if let Some(window) = open
        && window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let state = state.clone();
    cx.spawn(async move |cx| {
        let monitors = state.capture.list_monitors().await.unwrap_or_default();
        let hooks = Rc::new(settings_hooks(&state, monitors));
        let resume = hooks.apply_hotkeys.clone();
        let opened = cx.update(|cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(880.), px(640.)),
                    cx,
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Framecut Settings".into()),
                    ..Default::default()
                }),
                focus: true,
                show: true,
                kind: WindowKind::Normal,
                ..Default::default()
            };
            cx.open_window(options, move |window, cx| {
                Theme::sync_system_appearance(Some(window), cx);
                // The title bar's close button: re-register the hotkeys (closing
                // mid-recording must not leave them paused), and close the way
                // Escape does. GPUI's own close logs errors as the window goes.
                window.on_window_should_close(cx, move |window, cx| {
                    drop(resume());
                    popup::close_window(window, cx);
                    false
                });
                let view = cx.new(|cx| SettingsWindow::new(hooks, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        });
        match opened {
            Ok(window) => {
                log::info!("settings window opened");
                let hwnd = window
                    .update(cx, |_, window, _| popup::raw_hwnd(window))
                    .ok()
                    .flatten();
                if let Some(hwnd) = hwnd {
                    platform_window::show_normal(hwnd);
                }
                state.settings_window.replace(Some(window.into()));
            }
            Err(e) => log::error!("could not open the settings window: {e:#}"),
        }
    })
    .detach();
}

/// How the settings window reaches the rest of Framecut.
fn settings_hooks(state: &Rc<State>, monitors: Vec<MonitorInfo>) -> Hooks {
    let (changed, pause, apply, probe) =
        (state.clone(), state.clone(), state.clone(), state.clone());
    Hooks {
        settings: state.settings.clone(),
        changed: Rc::new(move |cx: &mut App| {
            let settings = changed.settings.borrow().clone();
            changed.save(&settings, cx.background_executor());
            changed.refresh_tray_menu();
            log::info!("settings changed");
        }),
        pause_hotkeys: Rc::new(move || {
            // The request is sent at once; nothing waits for the answer.
            drop(pause.platform.set_hotkeys(Vec::new()));
        }),
        apply_hotkeys: Rc::new(move || {
            let registering = apply.platform.set_hotkeys(apply.hotkeys());
            Box::pin(async move {
                registering
                    .await
                    .into_iter()
                    .map(|conflict| {
                        log::warn!("{} is taken by another application", conflict.hotkey);
                        conflict.id
                    })
                    .collect()
            })
        }),
        probe_hotkeys: Rc::new(move || {
            let every = hotkeys(&probe.settings.borrow(), true, true);
            let registering = probe.platform.set_hotkeys(every);
            let state = probe.clone();
            Box::pin(async move {
                let taken = registering
                    .await
                    .into_iter()
                    .map(|conflict| {
                        log::warn!("{} is taken by another application", conflict.hotkey);
                        conflict.id
                    })
                    .collect();
                // Back to what applies now; any clash there is in `taken`.
                drop(state.platform.set_hotkeys(state.hotkeys()).await);
                taken
            })
        }),
        launch_at_startup: Rc::new(framecut_platform::startup::launch_at_startup),
        set_launch_at_startup: Rc::new(|enabled| {
            match framecut_platform::startup::set_launch_at_startup(enabled) {
                Ok(()) => log::info!("launch at startup {}", if enabled { "on" } else { "off" }),
                Err(e) => log::error!("{e:#}"),
            }
        }),
        diagnostics: Diagnostics {
            version: env!("CARGO_PKG_VERSION").to_string(),
            windows_build: framecut_capture::display::windows_build(),
            monitors,
            log_dir: state.log_dir.clone(),
            settings_path: state.settings_path.clone(),
        },
    }
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
) -> Result<(), Failure> {
    let info = monitor_info(state, monitor).await?;
    let (mode, target) = {
        let settings = state.settings.borrow();
        (settings.last_mode, settings.last_target)
    };
    let (bar, outcome) = popup::open(
        &info,
        bar_rect(&info),
        Activation::Take,
        cx,
        move |window, cx| cx.new(|cx| CaptureBar::new(target, window, cx).with_mode(mode)),
    )
    .map_err(|e| Failure::new("Could not open the Capture Bar.", e))?;
    if let Some(hwnd) = bar.hwnd() {
        platform_window::round_corners(hwnd);
        // Never part of a screenshot, even if the screen is frozen while
        // the bar is still fading out.
        exclude_from_capture(hwnd, "Capture Bar");
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
        CaptureBarEvent::Chosen(mode, target) => {
            log::info!("Capture Bar: {mode:?} {target:?}");
            state.update_settings(
                |s| {
                    s.last_mode = mode;
                    s.last_target = target;
                },
                cx,
            );
            match mode {
                CaptureMode::Screenshot => {
                    capture(state, target, monitor, Instant::now(), cx).await
                }
                CaptureMode::Record => record(state, target, monitor, Instant::now(), cx).await,
            }
        }
    }
}

/// Keep one of Framecut's windows out of screenshots and recordings (PRD
/// §13.6). FRAMECUT_CAPTURABLE_UI leaves them capturable, for screenshots
/// of Framecut itself. Returns whether Windows confirmed the exclusion (or
/// it was left out on purpose).
fn exclude_from_capture(hwnd: isize, what: &str) -> bool {
    if std::env::var_os("FRAMECUT_CAPTURABLE_UI").is_some() {
        return true;
    }
    match platform_window::exclude_from_capture(hwnd) {
        Ok(()) => true,
        Err(e) => {
            log::warn!("could not exclude the {what} from capture: {e:#}");
            false
        }
    }
}

/// What Framecut knows of `monitor`.
async fn monitor_info(state: &State, monitor: MonitorId) -> Result<MonitorInfo, Failure> {
    state
        .capture
        .list_monitors()
        .await?
        .into_iter()
        .find(|m| m.id == monitor)
        .ok_or_else(|| Failure::plain("The monitor under the pointer is gone."))
}

/// Capture `target` on `monitor`.
async fn capture(
    state: &Rc<State>,
    target: CaptureTarget,
    monitor: MonitorId,
    pressed: Instant,
    cx: &mut AsyncApp,
) -> Result<(), Failure> {
    let taken_at = Local::now().naive_local();
    let include_cursor = state.settings.borrow().include_cursor;
    let frame = state
        .capture
        .freeze_monitor(monitor, include_cursor)
        .await?;
    let (width, height) = frame.size();
    let whole = PhysicalRect::new(0, 0, width, height);
    let mode = match target {
        CaptureTarget::Display => {
            let shot = state.capture.screenshot(&frame, whole).await;
            return deliver(state, shot?, frame.monitor(), pressed, taken_at, cx).await;
        }
        CaptureTarget::Area => Mode::Area,
        CaptureTarget::Window => Mode::Window,
    };
    let info = frame.monitor().clone();
    let event = select(state, &frame, mode, false, pressed, cx).await?;
    let released = Instant::now();
    let shot = match event {
        OverlayEvent::Cancelled => {
            log::info!("selection cancelled");
            return Ok(());
        }
        OverlayEvent::Selected(rect) => state.capture.screenshot(&frame, rect).await,
        OverlayEvent::Display => state.capture.screenshot(&frame, whole).await,
        OverlayEvent::Window { hwnd, visible } => {
            let include_cursor = state.settings.borrow().include_cursor;
            match state.capture.capture_window(hwnd, include_cursor).await {
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
    deliver(state, shot?, &info, released, taken_at, cx).await
}

/// Show the selection overlay over `frame`, in `mode` or, with
/// `recording`, to choose an area to record, and wait for the choice.
async fn select(
    state: &State,
    frame: &FrozenFrame,
    mode: Mode,
    recording: bool,
    pressed: Instant,
    cx: &mut AsyncApp,
) -> Result<OverlayEvent, Failure> {
    let frozen = pressed.elapsed();
    let info = frame.monitor().clone();
    let (width, height) = frame.size();
    let windows = screen_windows(info.bounds);
    let snap_to_windows = state.settings.borrow().snap_to_windows;
    let overlay_frame = OverlayFrame::from_bgra(
        width,
        height,
        info.scale_factor,
        frame.preview_bgra().to_vec(),
    );
    let (overlay, outcome) = popup::open(
        &info,
        info.bounds,
        Activation::Take,
        cx,
        move |window, cx| {
            cx.new(|cx| {
                let overlay = SelectionOverlay::new(overlay_frame, window, cx)
                    .with_windows(windows, snap_to_windows)
                    .with_mode(mode);
                if recording {
                    overlay.for_recording()
                } else {
                    overlay
                }
            })
        },
    )
    .map_err(|e| Failure::new("Could not open the selection screen.", e))?;
    // A screenshot's own screen is frozen before the overlay appears, but a
    // recording may be running (PRD §13.5), or about to start as it goes.
    if let Some(hwnd) = overlay.hwnd() {
        exclude_from_capture(hwnd, "selection overlay");
    }
    log::info!(
        "overlay up {} ms after the request (freeze {} ms)",
        pressed.elapsed().as_millis(),
        frozen.as_millis()
    );
    let mut outcome = outcome;
    let event = outcome.next().await.unwrap_or(OverlayEvent::Cancelled);
    overlay.close(cx);
    Ok(event)
}

/// The recording controls' distance from the recorded area, logical pixels.
const CONTROLS_GAP: f32 = 12.0;

/// Record `target` on `monitor`: choose the area on a frozen frame of it,
/// show the controls, then start the recorder. It runs until
/// [`stop_recording`] or [`discard_recording`].
async fn record(
    state: &Rc<State>,
    target: CaptureTarget,
    monitor: MonitorId,
    pressed: Instant,
    cx: &mut AsyncApp,
) -> Result<(), Failure> {
    if state.recording.borrow().is_some() {
        log::info!("already recording; the new request is ignored");
        return Ok(());
    }
    let (region, info) = match target {
        CaptureTarget::Display => (None, monitor_info(state, monitor).await?),
        CaptureTarget::Area | CaptureTarget::Window => {
            let frame = state.capture.freeze_monitor(monitor, false).await?;
            let region = match select(state, &frame, Mode::Area, true, pressed, cx).await? {
                OverlayEvent::Selected(rect) => Some(rect),
                OverlayEvent::Display => None,
                OverlayEvent::Window { visible, .. } => Some(visible),
                OverlayEvent::Cancelled => {
                    log::info!("recording cancelled");
                    return Ok(());
                }
            };
            (region, frame.monitor().clone())
        }
    };
    // The border shows what will be recorded: grey until recording starts.
    let countdown = state.settings.borrow().countdown();
    let waiting = if countdown > 0 {
        FRAME_WAITING
    } else {
        FRAME_RECORDING
    };
    let frame = show_frame(&info, region, waiting);
    if countdown > 0 && !count_down(&info, region, countdown, cx).await {
        log::info!("recording cancelled during the countdown");
        return Ok(());
    }
    let chosen = Instant::now();
    let clock = Rc::new(Cell::new(Clock::new(chosen)));
    // The controls come first, so they are excluded before the first frame.
    let keys = {
        let settings = state.settings.borrow();
        RecordKeys {
            pause: settings.pause_hotkey.clone(),
            stop: settings.record_hotkey.clone(),
            restart: settings.restart_hotkey.clone(),
            discard: settings.discard_hotkey.clone(),
            undo: settings.undo_hotkey.clone(),
        }
    };
    let (controls, requests) = match open_controls(&info, region, clock.clone(), keys, cx) {
        Some((controls, requests)) => (Some(controls), Some(requests)),
        None => (None, None),
    };

    let settings = state.settings.borrow().clone();
    let dir = settings.recording_dir();
    let (fps, include_cursor) = (settings.record_fps(), settings.record_cursor);
    let started_at = Local::now().naive_local();
    let started = cx
        .background_executor()
        .spawn(async move {
            let path = files::new_recording(&dir, started_at).map_err(|e| {
                Failure::new(
                    format!(
                        "Could not save to {}. Check the folder in Settings.",
                        dir.display()
                    ),
                    format!("{e:#}"),
                )
            })?;
            let options = RecordOptions {
                monitor,
                region,
                fps,
                include_cursor,
                path: files::partial_path(&path),
            };
            let recorder = Recorder::start(options.clone()).map_err(|e| {
                Failure::new("Could not start recording.", format!("recorder: {e:#}"))
            })?;
            Ok::<_, Failure>((recorder, path, options))
        })
        .await
        .map_err(Failure::of_recording);
    let (recorder, path, options) = match started {
        Ok(started) => started,
        Err(failure) => {
            if let Some(controls) = controls {
                controls.popup.close(cx);
            }
            return Err(failure);
        }
    };
    log::info!(
        "recording {} at {fps} fps, {} ms after the choice",
        match region {
            Some(r) => format!("{}×{} at {},{}", r.width, r.height, r.x, r.y),
            None => "the display".to_string(),
        },
        chosen.elapsed().as_millis()
    );
    clock.set(Clock::new(Instant::now()));
    let mut recorder = recorder;
    let ended = recorder.ended();
    let watched = path.clone();
    let recording = Recording {
        recorder: Some(recorder),
        path,
        options,
        clock,
        controls,
        asking: None,
        discarded: None,
        frame,
    };
    recording.refresh_controls(cx);
    recording.color_frame_for_clock();
    state.recording.replace(Some(recording));
    state.recording_changed(cx);
    if let Some(requests) = requests {
        handle_controls(state.clone(), requests, cx);
    }
    watch_recording(state, ended, watched, cx);
    Ok(())
}

/// Show the recording controls next to `region` (physical pixels relative
/// to the monitor; `None` for all of it), excluded from capture. If
/// Windows cannot exclude them and they would cover the region, they are
/// not shown (PRD §16); the hotkey and the tray still stop the recording.
fn open_controls(
    info: &MonitorInfo,
    region: Option<PhysicalRect>,
    clock: Rc<Cell<Clock>>,
    keys: RecordKeys,
    cx: &mut AsyncApp,
) -> Option<(Controls, UnboundedReceiver<RecordBarEvent>)> {
    let (b, scale) = (info.bounds, info.scale_factor);
    let recorded = framecut_capture::record::recordable(region, b.width, b.height);
    let recorded = PhysicalRect::new(
        b.x + recorded.x,
        b.y + recorded.y,
        recorded.width,
        recorded.height,
    );
    let work = platform_window::work_area(info.id.0)
        .map(|(x, y, w, h)| PhysicalRect::new(x, y, w, h))
        .unwrap_or(b);
    let size = (
        (RECORD_BAR_WIDTH * scale).round() as u32,
        (RECORD_BAR_HEIGHT * scale).round() as u32,
    );
    let gap = (CONTROLS_GAP * scale).round() as u32;
    let (rect, covers) = recording::controls_rect(work, recorded, size, gap);

    let view = Rc::new(RefCell::new(None));
    let slot = view.clone();
    let opened = popup::open(info, rect, Activation::OnClick, cx, move |window, cx| {
        let bar = cx.new(|cx| RecordBar::new(clock, keys, window, cx));
        slot.replace(Some(bar.clone()));
        bar
    });
    let (popup, requests) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            log::error!("could not show the recording controls: {e}");
            return None;
        }
    };
    let view = view.take()?;
    let excluded = match popup.hwnd() {
        Some(hwnd) => {
            platform_window::round_corners(hwnd);
            exclude_from_capture(hwnd, "recording controls")
        }
        None => false,
    };
    if !excluded && covers {
        log::warn!("the recording controls would be recorded; recording without them");
        popup.close(cx);
        return None;
    }
    log::info!(
        "recording controls at {},{} ({}, {})",
        rect.x,
        rect.y,
        if covers {
            "over the area"
        } else {
            "outside the area"
        },
        if excluded {
            "excluded from capture"
        } else {
            "not excluded"
        }
    );
    Some((Controls { popup, view }, requests))
}

/// Notice when the take written to `path` ends by itself (the display went
/// away, the device was lost, the disk filled up: PRD §25) and finish it as
/// if stopped: what was recorded is saved, and the notification says why.
/// `ended` is the recorder's [`Recorder::ended`]; it also resolves when the
/// take is stopped, discarded or restarted, and then the take is no longer
/// the one recording.
fn watch_recording(
    state: &Rc<State>,
    ended: Option<impl std::future::Future<Output = ()> + 'static>,
    path: PathBuf,
    cx: &mut AsyncApp,
) {
    let Some(ended) = ended else {
        return;
    };
    let state = state.clone();
    cx.spawn(async move |cx| {
        ended.await;
        let by_itself = state
            .recording
            .borrow()
            .as_ref()
            .is_some_and(|r| r.path == path && r.recorder.is_some());
        if by_itself {
            log::warn!("the recording ended by itself");
            stop_recording(&state, cx);
        }
    })
    .detach();
}

/// Windows says the displays changed: if the recorded one is gone, end the
/// recording with what was recorded. Other changes leave it alone.
fn displays_changed(state: &State) {
    let recording = state.recording.borrow();
    let Some(recording) = recording.as_ref() else {
        return;
    };
    let Some(recorder) = &recording.recorder else {
        return;
    };
    if framecut_capture::record::attached(recording.options.monitor) {
        log::info!("displays changed; the recorded one is still attached");
    } else {
        log::warn!("the recorded display is gone");
        recorder.display_gone();
    }
}

/// Count down `seconds` over the middle of `region` (physical pixels
/// relative to the monitor; `None` for all of it) before recording starts.
/// The countdown takes the keyboard, so Enter and Escape work, and gives it
/// back when it ends. Returns whether to start.
async fn count_down(
    info: &MonitorInfo,
    region: Option<PhysicalRect>,
    seconds: u32,
    cx: &mut AsyncApp,
) -> bool {
    let (b, scale) = (info.bounds, info.scale_factor);
    let area = framecut_capture::record::recordable(region, b.width, b.height);
    let (w, h) = (
        (COUNTDOWN_WIDTH * scale).round() as i32,
        (COUNTDOWN_HEIGHT * scale).round() as i32,
    );
    let centre = |start: i32, side: u32, size: i32, limit: u32| {
        (start + (side as i32 - size) / 2).clamp(0, (limit as i32 - size).max(0))
    };
    let rect = PhysicalRect::new(
        b.x + centre(area.x, area.width, w, b.width),
        b.y + centre(area.y, area.height, h, b.height),
        w as u32,
        h as u32,
    );
    let give_back = platform_window::foreground_window();
    let opened = popup::open(info, rect, Activation::Take, cx, move |window, cx| {
        cx.new(|cx| Countdown::new(seconds, window, cx))
    });
    let (popup, mut events) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            log::warn!("could not show the countdown; recording at once: {e}");
            return true;
        }
    };
    if let Some(hwnd) = popup.hwnd() {
        platform_window::round_corners(hwnd);
        exclude_from_capture(hwnd, "countdown");
    }
    let event = events.next().await.unwrap_or(CountdownEvent::Cancel);
    popup.close(cx);
    if let Some(window) = give_back {
        platform_window::bring_to_front(window);
    }
    event == CountdownEvent::Go
}

/// Show the dashed border around `region` (physical pixels relative to the
/// monitor; `None` for all of it) in `color`, before recording starts, so
/// it is excluded from the first frame.
fn show_frame(info: &MonitorInfo, region: Option<PhysicalRect>, color: [u8; 3]) -> Option<Frame> {
    let (b, scale) = (info.bounds, info.scale_factor);
    let area = framecut_capture::record::recordable(region, b.width, b.height);
    let px = |logical: f32| ((logical * scale).round() as u32).max(1);
    let style = FrameStyle {
        thickness: px(FRAME_THICKNESS),
        dash: px(FRAME_DASH),
        gap: px(FRAME_GAP),
    };
    let area = FrameRect::new(b.x + area.x, b.y + area.y, area.width, area.height);
    let bounds = FrameRect::new(b.x, b.y, b.width, b.height);
    match Frame::show(area, bounds, style, color) {
        Ok((frame, 0)) => Some(frame),
        Ok((frame, missing)) => {
            log::warn!("{missing} sides of the recording border would be recorded; left out");
            Some(frame)
        }
        Err(e) => {
            log::warn!("could not show the recording border: {e:#}");
            None
        }
    }
}

/// Do what the recording controls ask until they close.
fn handle_controls(
    state: Rc<State>,
    mut requests: UnboundedReceiver<RecordBarEvent>,
    cx: &mut AsyncApp,
) {
    cx.spawn(async move |cx| {
        while let Some(asked) = requests.next().await {
            match asked {
                RecordBarEvent::TogglePause => toggle_pause(&state, cx),
                RecordBarEvent::Stop => stop_recording(&state, cx),
                RecordBarEvent::Restart => request(&state, Destructive::Restart, cx).await,
                RecordBarEvent::Discard => request(&state, Destructive::Discard, cx).await,
                RecordBarEvent::Confirm => confirm(&state, cx).await,
                RecordBarEvent::Cancel => keep_take(&state, cx),
                RecordBarEvent::Undo => undo(&state, cx),
            }
            if state.recording.borrow().is_none() {
                break;
            }
        }
    })
    .detach();
}

/// Pause the recording, or resume it if it is paused. While the controls
/// ask before throwing the take away, this answers "keep it"; while a
/// discard can be undone, it does nothing.
fn toggle_pause(state: &State, cx: &mut AsyncApp) {
    let (asking, discarded) = match &*state.recording.borrow() {
        Some(r) => (r.asking.is_some(), r.discarded.is_some()),
        None => return,
    };
    if asking {
        return keep_take(state, cx);
    }
    if discarded {
        return;
    }
    let view = {
        let recording = state.recording.borrow();
        let Some(recording) = recording.as_ref() else {
            return;
        };
        let paused = !recording.clock.get().is_paused();
        recording.set_paused(paused);
        let clock = recording.clock.get();
        if paused {
            log::info!(
                "recording paused at {}",
                recording::clock(clock.elapsed(Instant::now()))
            );
        } else {
            log::info!("recording resumed");
        }
        recording.view()
    };
    show(view, cx, |_, cx| cx.notify());
    state.refresh_tray_menu();
}

/// Discard or Restart was asked for: ask first, or act at once and offer
/// undo, as the settings say. Asking for the same action again while the
/// controls ask confirms it.
async fn request(state: &Rc<State>, action: Destructive, cx: &mut AsyncApp) {
    let (asking, discarded) = match &*state.recording.borrow() {
        Some(r) => (r.asking.map(|a| a.action), r.discarded.is_some()),
        None => return,
    };
    match asking {
        Some(asked) if asked == action => return confirm(state, cx).await,
        Some(_) => return,
        None => {}
    }
    // Discarding a discarded take again removes it at once; restarting it
    // means nothing.
    if discarded {
        if action == Destructive::Discard {
            confirm(state, cx).await;
        }
        return;
    }
    // A restart's previous take is still on offer.
    if action == Destructive::Restart && state.previous_take.borrow().is_some() {
        return;
    }
    let confirm_first = state.settings.borrow().confirm_discard;
    match (confirm_first, action) {
        (true, _) => ask(state, action, cx),
        (false, Destructive::Discard) => discard_with_undo(state, cx),
        (false, Destructive::Restart) => restart_recording(state, true, cx).await,
    }
}

/// Pause and ask before `action` throws the take away. The controls take
/// the keyboard so Enter or Escape answers; it goes back afterwards.
fn ask(state: &State, action: Destructive, cx: &mut AsyncApp) {
    let (view, hwnd, length) = {
        let mut slot = state.recording.borrow_mut();
        let Some(recording) = slot.as_mut() else {
            return;
        };
        if recording.recorder.is_none() {
            return;
        }
        let resume = recording.set_paused(true);
        let hwnd = recording.controls.as_ref().and_then(|c| c.popup.hwnd());
        let give_back = platform_window::foreground_window().filter(|w| Some(*w) != hwnd);
        recording.asking = Some(Asking {
            action,
            resume,
            give_back,
        });
        let length = recording.clock.get().elapsed(Instant::now());
        (recording.view(), hwnd, length)
    };
    log::info!(
        "asking before {action:?} of a {} take",
        recording::clock(length)
    );
    show(view, cx, |bar, cx| {
        bar.set_mode(BarMode::Confirm(action, length), cx)
    });
    if let Some(hwnd) = hwnd {
        platform_window::bring_to_front(hwnd);
    }
    state.refresh_tray_menu();
}

/// Stop asking: the controls show the actions again and the keyboard goes
/// back to where it was. Returns what was being asked.
fn stop_asking(state: &State, cx: &mut AsyncApp) -> Option<Asking> {
    let (asking, view) = {
        let mut slot = state.recording.borrow_mut();
        let recording = slot.as_mut()?;
        (recording.asking.take()?, recording.view())
    };
    show(view, cx, |bar, cx| bar.set_mode(BarMode::Controls, cx));
    if let Some(window) = asking.give_back {
        platform_window::bring_to_front(window);
    }
    Some(asking)
}

/// Yes: throw the take away as asked, or a discarded one at once rather
/// than when its undo window closes.
async fn confirm(state: &Rc<State>, cx: &mut AsyncApp) {
    let Some(asking) = stop_asking(state, cx) else {
        let discarded = state
            .recording
            .borrow()
            .as_ref()
            .is_some_and(|r| r.discarded.is_some());
        if discarded {
            discard_recording(state, cx);
        }
        return;
    };
    match asking.action {
        Destructive::Discard => discard_recording(state, cx),
        Destructive::Restart => restart_recording(state, false, cx).await,
    }
}

/// No: keep the take, and carry on recording if it was running.
fn keep_take(state: &State, cx: &mut AsyncApp) {
    let Some(asking) = stop_asking(state, cx) else {
        return;
    };
    let view = {
        let recording = state.recording.borrow();
        let Some(recording) = recording.as_ref() else {
            return;
        };
        if asking.resume {
            recording.set_paused(false);
        }
        recording.view()
    };
    log::info!("{:?} cancelled; the take is kept", asking.action);
    show(view, cx, |_, cx| cx.notify());
    state.refresh_tray_menu();
}

/// Discard at once, but pause rather than delete for the undo window, so
/// Undo can bring the take back.
/// The controls take the keyboard meanwhile, so Enter can discard at once;
/// it goes back afterwards.
fn discard_with_undo(state: &Rc<State>, cx: &mut AsyncApp) {
    let generation = state.next_generation();
    let window = state.settings.borrow().undo_window();
    let until = Instant::now() + window;
    let (view, hwnd) = {
        let mut slot = state.recording.borrow_mut();
        let Some(recording) = slot.as_mut() else {
            return;
        };
        if recording.recorder.is_none() {
            return;
        }
        let resume = recording.set_paused(true);
        recording.color_frame(FRAME_DISCARDED);
        let hwnd = recording.controls.as_ref().and_then(|c| c.popup.hwnd());
        let give_back = platform_window::foreground_window().filter(|w| Some(*w) != hwnd);
        recording.discarded = Some(Discarded {
            resume,
            generation,
            give_back,
        });
        (recording.view(), hwnd)
    };
    log::info!(
        "recording discarded; it can be undone for {} s",
        window.as_secs()
    );
    show(view, cx, |bar, cx| {
        bar.set_mode(BarMode::Discarded { until }, cx)
    });
    if let Some(hwnd) = hwnd {
        platform_window::bring_to_front(hwnd);
    }
    state.recording_changed(cx);
    let state = state.clone();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(window).await;
        let due = state
            .recording
            .borrow()
            .as_ref()
            .and_then(|r| r.discarded)
            .is_some_and(|d| d.generation == generation);
        if due {
            discard_recording(&state, cx);
        }
    })
    .detach();
}

/// Undo a discard, or keep the take a restart replaced.
fn undo(state: &Rc<State>, cx: &mut AsyncApp) {
    let discarded = {
        let mut slot = state.recording.borrow_mut();
        slot.as_mut()
            .and_then(|r| Some((r.discarded.take()?, r.view())))
    };
    if let Some((discarded, view)) = discarded {
        if let Some(recording) = state.recording.borrow().as_ref() {
            if discarded.resume {
                recording.set_paused(false);
            }
            recording.color_frame_for_clock();
        }
        if let Some(window) = discarded.give_back {
            platform_window::bring_to_front(window);
        }
        log::info!("discard undone");
        show(view, cx, |bar, cx| {
            bar.set_mode(BarMode::Controls, cx);
            bar.notice("Discard undone", Duration::from_secs(2), cx);
        });
        state.recording_changed(cx);
        return;
    }
    let Some(previous) = state.previous_take.take() else {
        return;
    };
    let view = state.recording.borrow().as_ref().and_then(Recording::view);
    let name = previous
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    state.recording_changed(cx);
    let state = state.clone();
    cx.spawn(async move |cx| {
        let path = previous.path.clone();
        keep_previous_take(previous, cx).await;
        match view {
            Some(view) => {
                // A notification now would be in the recording.
                show(Some(view), cx, |bar, cx| {
                    bar.offer_previous(None, cx);
                    bar.notice(format!("Kept {name}"), Duration::from_secs(3), cx);
                })
            }
            None => state.notify("Previous take kept", name, Some(path)),
        }
    })
    .detach();
}

/// Give a take a restart replaced its final name.
async fn keep_previous_take(previous: PreviousTake, cx: &mut AsyncApp) {
    let path = previous.path;
    let kept = cx
        .background_executor()
        .spawn({
            let path = path.clone();
            async move { std::fs::rename(files::partial_path(&path), &path) }
        })
        .await;
    match kept {
        Ok(()) => log::info!("kept the previous take as {}", path.display()),
        Err(e) => log::error!("could not keep {}: {e}", path.display()),
    }
}

/// Take the recording in progress and its recorder, closing the controls.
/// `None` if there is none, or while a restart is swapping recorders.
fn end_recording(state: &State, cx: &mut AsyncApp) -> Option<(Recorder, PathBuf)> {
    if state.recording.borrow().as_ref()?.recorder.is_none() {
        log::info!("the recording is restarting; the request is ignored");
        return None;
    }
    // Hand the keyboard back if the controls were asking or counting down.
    stop_asking(state, cx);
    let mut recording = state.recording.borrow_mut().take()?;
    recording.close_controls(cx);
    if let Some(window) = recording.discarded.and_then(|d| d.give_back) {
        platform_window::bring_to_front(window);
    }
    state.recording_changed(cx);
    Some((recording.recorder?, recording.path))
}

/// Stop the recording in progress, finish its file, and say where it is.
/// Stopping always keeps the take, even one the controls were asking about
/// or one discarded moments ago.
fn stop_recording(state: &Rc<State>, cx: &mut AsyncApp) {
    let Some((recorder, path)) = end_recording(state, cx) else {
        return;
    };
    let state = state.clone();
    cx.spawn(async move |cx| {
        let result = finish_recording(recorder, path, cx).await;
        report_recording(&state, result);
    })
    .detach();
}

/// Stop the recording in progress and delete it: no file is left (PRD
/// §7.7).
fn discard_recording(state: &Rc<State>, cx: &mut AsyncApp) {
    let Some((recorder, path)) = end_recording(state, cx) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            let partial = files::partial_path(&path);
            if let Err(e) = recorder.stop() {
                log::warn!("the discarded recording did not stop cleanly: {e:#}");
            }
            match std::fs::remove_file(&partial) {
                Ok(()) => log::info!("recording discarded"),
                Err(e) => log::error!("could not delete {}: {e}", partial.display()),
            }
        })
        .detach();
}

/// Start again with the same area and settings, as a new file (PRD §7.7).
/// With `keep_previous`, the take it replaces is finished and kept for
/// the undo window, so Undo can keep it; otherwise it is deleted.
async fn restart_recording(state: &Rc<State>, keep_previous: bool, cx: &mut AsyncApp) {
    let taken = state.recording.borrow_mut().as_mut().and_then(|recording| {
        let recorder = recording.recorder.take()?;
        Some((recorder, recording.options.clone()))
    });
    let Some((recorder, options)) = taken else {
        return;
    };
    let dir = state.settings.borrow().recording_dir();
    let started_at = Local::now().naive_local();
    let restarted = cx
        .background_executor()
        .spawn(async move {
            let finished = recorder.stop();
            if let Err(e) = &finished {
                log::warn!("the replaced take did not stop cleanly: {e:#}");
            }
            let kept = keep_previous && finished.is_ok();
            if !kept {
                let _ = std::fs::remove_file(&options.path);
            }
            let path = files::new_recording(&dir, started_at)?;
            let options = RecordOptions {
                path: files::partial_path(&path),
                ..options
            };
            let recorder = Recorder::start(options.clone())?;
            anyhow::Ok((recorder, path, options, kept))
        })
        .await;
    let mut slot = state.recording.borrow_mut();
    match (restarted, slot.as_mut()) {
        (Ok((mut recorder, path, options, kept)), Some(recording)) => {
            let ended = recorder.ended();
            let watched = path.clone();
            let previous = std::mem::replace(&mut recording.path, path);
            recording.options = options;
            recording.recorder = Some(recorder);
            recording.clock.set(Clock::new(Instant::now()));
            let view = recording.view();
            drop(slot);
            log::info!("recording restarted");
            let until = Instant::now() + state.settings.borrow().undo_window();
            if kept {
                let generation = state.next_generation();
                state.previous_take.replace(Some(PreviousTake {
                    path: previous,
                    generation,
                }));
                forget_previous_take_later(
                    state,
                    generation,
                    until.saturating_duration_since(Instant::now()),
                    cx,
                );
            }
            show(view, cx, |bar, cx| {
                bar.offer_previous(kept.then_some(until), cx);
                bar.notice("Restarted", Duration::from_secs(2), cx);
            });
            state.recording_changed(cx);
            watch_recording(state, ended, watched, cx);
        }
        // Framecut quit while restarting: the new recording is not wanted.
        (Ok((recorder, ..)), None) => {
            drop(slot);
            cx.background_executor()
                .spawn(async move {
                    if let Ok(summary) = recorder.stop() {
                        let _ = std::fs::remove_file(summary.path);
                    }
                })
                .detach();
        }
        (Err(e), _) => {
            let recording = slot.take();
            drop(slot);
            if let Some(mut recording) = recording {
                recording.close_controls(cx);
            }
            state.recording_changed(cx);
            let failure = Failure::new("Could not restart recording.", format!("restart: {e:#}"))
                .of_recording();
            log::error!("{}", failure.detail);
            state.notify(failure.title(), failure.message, None);
        }
    }
}

/// Delete the take a restart replaced once its undo window closes, unless
/// Undo kept it or a newer window replaced it.
fn forget_previous_take_later(
    state: &Rc<State>,
    generation: u64,
    window: Duration,
    cx: &mut AsyncApp,
) {
    let state = state.clone();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(window).await;
        let due = state
            .previous_take
            .borrow()
            .as_ref()
            .is_some_and(|p| p.generation == generation);
        if !due {
            return;
        }
        let Some(previous) = state.previous_take.take() else {
            return;
        };
        let view = state.recording.borrow().as_ref().and_then(Recording::view);
        show(view, cx, |bar, cx| bar.offer_previous(None, cx));
        state.recording_changed(cx);
        let partial = files::partial_path(&previous.path);
        match std::fs::remove_file(&partial) {
            Ok(()) => log::info!("the replaced take is deleted"),
            Err(e) => log::error!("could not delete {}: {e}", partial.display()),
        }
    })
    .detach();
}

/// Stop `recorder` and move the file to `path`, its final name, off the
/// main thread. On failure, the unfinished file is deleted: it cannot be
/// played.
async fn finish_recording(
    recorder: Recorder,
    path: PathBuf,
    cx: &mut AsyncApp,
) -> Result<RecordingSummary, Failure> {
    cx.background_executor()
        .spawn(async move {
            let partial = files::partial_path(&path);
            let summary = recorder.stop().and_then(|summary| {
                std::fs::rename(&partial, &path)?;
                Ok(summary)
            });
            match summary {
                Ok(summary) => Ok(RecordingSummary { path, ..summary }),
                Err(e) => {
                    let _ = std::fs::remove_file(&partial);
                    Err(Failure::new(
                        "The recording could not be finished.",
                        format!("finishing {}: {e:#}", path.display()),
                    )
                    .of_recording())
                }
            }
        })
        .await
}

/// Log a finished recording and tell the user where it went.
fn report_recording(state: &State, result: Result<RecordingSummary, Failure>) {
    match result {
        Ok(summary) => {
            let name = summary
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            log::info!(
                "saved {name}: {}×{}, {:.2} s, {} frames ({} dropped, {} skipped), {:.1} s paused, {} encoder",
                summary.width,
                summary.height,
                summary.duration.as_secs_f64(),
                summary.frames,
                summary.dropped_busy,
                summary.skipped_rate,
                summary.paused.as_secs_f64(),
                if summary.hardware_encoder {
                    "hardware"
                } else {
                    "software"
                }
            );
            let length = recording::clock(summary.duration);
            match &summary.interrupted {
                // Unexpected, so always said, whatever the settings.
                Some(why) => {
                    log::warn!("the recording ended by itself: {why:?}");
                    state.notify(
                        format!("Recording stopped: {}", why.describe()),
                        format!("What was recorded is saved: {length}, {name}."),
                        Some(summary.path.clone()),
                    );
                }
                None if state.settings.borrow().notify_after_recording => {
                    state.notify(
                        format!("Recording saved · {length}"),
                        format!("{name}, {} × {}", summary.width, summary.height),
                        Some(summary.path.clone()),
                    );
                }
                None => {}
            }
        }
        // Failures are always reported.
        Err(failure) => {
            log::error!("{}", failure.detail);
            state.notify(failure.title(), failure.message, None);
        }
    }
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
) -> Result<(), Failure> {
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
    // PRD §24: say what happened and what to do; the causes go to the log.
    let busy = "Another app is holding the clipboard; try again in a moment.";
    let folder = settings.output_dir();
    let unwritable = format!(
        "Could not save to {}. Check the folder in Settings.",
        folder.display()
    );
    let result = match (copied, saved) {
        (Some(Err(copy)), Some(Err(save))) => Err(Failure::new(
            format!("Could not copy or save the screenshot. {unwritable}"),
            format!("copy: {copy:#}; save: {save:#}"),
        )),
        (Some(Err(copy)), Some(Ok(_))) => Err(Failure::new(
            format!("The screenshot was saved, but not copied. {busy}"),
            format!("copy: {copy:#}"),
        )),
        (Some(Err(copy)), None) => Err(Failure::new(
            format!("The screenshot was not copied. {busy}"),
            format!("copy: {copy:#}"),
        )),
        (Some(Ok(())), Some(Err(save))) => Err(Failure::new(
            format!("The screenshot was copied, but not saved. {unwritable}"),
            format!("save: {save:#}"),
        )),
        (None, Some(Err(save))) => Err(Failure::new(
            format!("The screenshot was not saved. {unwritable}"),
            format!("save: {save:#}"),
        )),
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
        state.notify(
            format!("Screenshot {width} × {height}"),
            message,
            saved_path.clone(),
        );
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
        let opened = popup::open(&monitor, rect, Activation::Never, cx, move |_, cx| {
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
            exclude_from_capture(hwnd, "thumbnail");
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
        let menu = tray_menu(&settings, TrayRecording::Idle);
        assert!(auto_save_checked(&menu));
        assert!(menu.contains(&MenuItem::item(MENU_CAPTURE_BAR, "Capture Bar\tCtrl+Alt+C")));
        assert!(menu.contains(&MenuItem::item(
            MENU_SCREENSHOT,
            "Screenshot an area\tCtrl+Alt+S"
        )));
        assert!(menu.contains(&MenuItem::item(MENU_RECORD, "Record an area\tCtrl+Alt+R")));
        let pause_item = |menu: &[MenuItem]| {
            menu.iter()
                .find(|item| matches!(item, MenuItem::Item { id: MENU_PAUSE, .. }))
                .cloned()
        };
        assert_eq!(pause_item(&menu), None);
        // While recording, the same item stops it, and pausing appears.
        let running = tray_menu(&settings, TrayRecording::Running);
        assert!(running.contains(&MenuItem::item(MENU_RECORD, "Stop recording\tCtrl+Alt+R")));
        assert_eq!(
            pause_item(&running),
            Some(MenuItem::item(MENU_PAUSE, "Pause recording\tCtrl+Alt+P"))
        );
        assert_eq!(
            pause_item(&tray_menu(&settings, TrayRecording::Paused)),
            Some(MenuItem::item(MENU_PAUSE, "Resume recording\tCtrl+Alt+P"))
        );
        let off = Settings {
            auto_save: false,
            ..settings
        };
        assert!(!auto_save_checked(&tray_menu(&off, TrayRecording::Idle)));
    }

    #[test]
    fn recording_chords_are_taken_only_while_they_mean_something() {
        let settings = Settings::default();
        let ids = |recording, undo| {
            hotkeys(&settings, recording, undo)
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>()
        };
        let during = [PAUSE_HOTKEY, RESTART_HOTKEY, DISCARD_HOTKEY];
        let idle = ids(false, false);
        assert!(idle.contains(&RECORD_HOTKEY));
        assert!(during.iter().all(|id| !idle.contains(id)));
        assert!(!idle.contains(&UNDO_HOTKEY));
        let recording = ids(true, false);
        assert!(during.iter().all(|id| recording.contains(id)));
        assert!(!recording.contains(&UNDO_HOTKEY));
        // Undo also outlives the recording: a restart's previous take can
        // be kept after Stop.
        assert!(ids(true, true).contains(&UNDO_HOTKEY));
        assert!(ids(false, true).contains(&UNDO_HOTKEY));
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
    fn capture_failures_tell_the_user_the_message_and_the_log_the_rest() {
        let failure = Failure::from(framecut_capture::CaptureError {
            code: framecut_capture::CaptureErrorCode::DeviceLost,
            message: "The graphics device was reset; try again".into(),
            detail: Some("CopySubresourceRegion failed: 0x887A0005".into()),
        });
        assert_eq!(failure.message, "The graphics device was reset; try again");
        assert!(failure.detail.contains("DeviceLost"), "{}", failure.detail);
        assert!(failure.detail.contains("0x887A0005"), "{}", failure.detail);
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
            adapter: String::new(),
        };
        let rect = bar_rect(&monitor);
        assert_eq!((rect.width, rect.height), (468, 198));
        assert_eq!((rect.x, rect.y), (3840 + (3840 - 468) / 2, 36));
    }
}
