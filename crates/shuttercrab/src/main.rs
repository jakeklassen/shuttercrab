// Release builds are a windowed app on Windows, with no console behind them.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use futures::channel::mpsc::UnboundedReceiver;
use shuttercrab::{
    app::{self, QUIT_HOTKEY, Shuttercrab},
    logging,
    settings::{self, Loaded, Settings},
    update::{UpdateBackend, Velopack},
};
use shuttercrab_capture::{Capture, display::windows_build};
use shuttercrab_platform::{Hotkey, Platform, PlatformEvent, Tray, startup};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Keeps settings and logs in this folder instead of the user's, for tests.
const DATA_DIR_VARIABLE: &str = "SHUTTERCRAB_DATA_DIR";

/// Counts the Rust heap for the memory lines in the log.
#[global_allocator]
static HEAP: shuttercrab::heap::Counting = shuttercrab::heap::Counting;

/// A tray notification for the user: its title and message.
type Notice = (String, String);

fn main() {
    // The recording helper: no tray, no UI, no single-instance check.
    if std::env::args().nth(1).as_deref() == Some(shuttercrab::recorder_process::FLAG) {
        std::process::exit(shuttercrab::recorder_process::serve());
    }
    // Next, before anything else: Velopack may run an install or uninstall
    // hook here and exit, or apply a downloaded update and restart. The
    // recording helper above never does, so an update can't end a
    // recording.
    velopack::VelopackApp::build()
        .on_before_uninstall_fast_callback(|_| {
            // Don't leave a startup entry pointing at a deleted program.
            let _ = startup::set_launch_at_startup(false);
        })
        .run();
    // Release builds have no console; print to the terminal that started us.
    #[cfg(not(debug_assertions))]
    shuttercrab_platform::attach_to_parent_terminal();

    let Some(_instance) = shuttercrab_platform::single_instance("Shuttercrab") else {
        // The log file belongs to the running instance; leave it alone.
        eprintln!("Shuttercrab is already running.");
        shuttercrab_platform::signal_running_instance();
        return;
    };
    // Started by Windows at sign-in: the tray only. Every other start opens
    // the window, like any app.
    let background = std::env::args().any(|a| a == startup::BACKGROUND_FLAG);
    let data_dir = std::env::var_os(DATA_DIR_VARIABLE).map(PathBuf::from);
    let log_file = start_logging(data_dir.as_deref());

    let settings_path = match &data_dir {
        Some(dir) => Some(dir.join("settings.json")),
        None => settings::default_path(),
    };
    let mut notices = Vec::new();
    let (mut settings, first_run) = load_settings(settings_path.as_deref(), &mut notices);
    tidy_up();
    normalize_hotkeys(&mut settings, &mut notices);

    let capture = match Capture::start() {
        Ok(capture) => capture,
        Err(e) => {
            log::error!("could not start capture: {e:#}");
            std::process::exit(1);
        }
    };
    capture.warm_up();
    let (platform, events) = start_platform(&settings, first_run, &mut notices);
    // One at a time: Windows shows only the latest tray notification.
    if let Some((title, message)) = notices.into_iter().next() {
        platform.notify(title, message);
    }
    log::info!(
        "ready: {} opens the Capture Bar, {} takes an area screenshot, {} records an area, {} quits",
        settings.capture_bar_hotkey,
        settings.screenshot_hotkey,
        settings.record_hotkey,
        app::QUIT_KEYS
    );
    let updates = Velopack::new().map(|velopack| {
        log::info!(
            "installed version {}; updates come from GitHub Releases",
            velopack.version()
        );
        Arc::new(velopack) as Arc<dyn UpdateBackend>
    });
    #[cfg(not(debug_assertions))]
    release_terminal(log_file.as_deref());

    gpui_kit::application()
        .with_assets(shuttercrab::icons::Icons)
        .run(move |cx| {
            gpui_kit::init(cx);
            app::run(
                Shuttercrab {
                    capture,
                    platform,
                    settings,
                    settings_path,
                    log_dir: log_file.as_ref().and_then(|f| f.parent().map(Into::into)),
                    updates,
                    open_window: !background,
                },
                events,
                cx,
            );
        });
}

/// Start the log, in `data_dir` when one is given. Returns the log file, if
/// it could be opened.
fn start_logging(data_dir: Option<&Path>) -> Option<PathBuf> {
    let log_dir = match data_dir {
        Some(dir) => Some(dir.join("logs")),
        None => logging::default_dir(),
    };
    let log_file = logging::init(log_dir.as_deref());
    log::info!(
        "Shuttercrab {} on Windows build {}",
        env!("CARGO_PKG_VERSION"),
        windows_build()
    );
    match &log_file {
        Some(path) => log::info!("logging to {}", path.display()),
        None => log::warn!("no log file; logging to the terminal only"),
    }
    log_file
}

/// The settings at `path`, or the defaults when they cannot be read (with a
/// notice when a broken file was set aside), and whether this is the first
/// run.
fn load_settings(path: Option<&Path>, notices: &mut Vec<Notice>) -> (Settings, bool) {
    let loaded = match path.map(settings::load) {
        Some(Ok((settings, loaded))) => {
            match &loaded {
                Loaded::Read => {}
                Loaded::Created => log::info!("created default settings"),
                Loaded::Replaced(bad) => {
                    log::warn!(
                        "settings could not be read; kept them as {} and used defaults",
                        bad.display()
                    );
                    notices.push((
                        "Settings were reset".to_string(),
                        format!(
                            "The old file could not be read and was kept as {}.",
                            bad.display()
                        ),
                    ));
                }
            }
            (settings, loaded == Loaded::Created)
        }
        Some(Err(e)) => {
            log::error!("could not load settings, using defaults: {e:#}");
            (Settings::default(), false)
        }
        None => {
            log::warn!("no settings folder; settings will not be saved");
            (Settings::default(), false)
        }
    };
    if let Some(path) = path {
        log::info!("settings in {}", path.display());
    }
    loaded
}

/// Bring the startup entry up to date, and remove old screenshots written
/// only for the thumbnail to open or drag.
fn tidy_up() {
    match startup::add_background_flag() {
        Ok(true) => log::info!("the startup entry now starts Shuttercrab in the tray"),
        Ok(false) => {}
        Err(e) => log::warn!("could not update the startup entry: {e:#}"),
    }
    let removed = shuttercrab::files::remove_old(
        &shuttercrab::files::temp_dir(),
        std::time::Duration::from_secs(24 * 60 * 60),
    );
    if removed > 0 {
        log::info!("removed {removed} temporary screenshots older than a day");
    }
}

/// Keep valid hotkeys in Shuttercrab's spelling; replace invalid ones with
/// the defaults, with a notice.
fn normalize_hotkeys(settings: &mut Settings, notices: &mut Vec<Notice>) {
    let defaults = Settings::default();
    let mut normalize = |value: &mut String, fallback: &str| match Hotkey::parse(value) {
        Ok(hotkey) => *value = hotkey.to_string(),
        Err(e) => {
            log::error!("hotkey {value:?} is not valid ({e:#}); using {fallback}");
            notices.push((
                "Hotkey not recognised".to_string(),
                format!("\"{value}\" is not a valid hotkey, so {fallback} is used instead."),
            ));
            *value = fallback.to_string();
        }
    };
    normalize(
        &mut settings.capture_bar_hotkey,
        &defaults.capture_bar_hotkey,
    );
    normalize(&mut settings.screenshot_hotkey, &defaults.screenshot_hotkey);
    normalize(&mut settings.record_hotkey, &defaults.record_hotkey);
    normalize(&mut settings.pause_hotkey, &defaults.pause_hotkey);
    normalize(&mut settings.restart_hotkey, &defaults.restart_hotkey);
    normalize(&mut settings.discard_hotkey, &defaults.discard_hotkey);
    normalize(&mut settings.undo_hotkey, &defaults.undo_hotkey);
}

/// Start the platform thread with the hotkeys and the tray icon. Adds a
/// notice for each hotkey another app owns, and the welcome on a first run
/// without any. Exits if the thread cannot start.
fn start_platform(
    settings: &Settings,
    first_run: bool,
    notices: &mut Vec<Notice>,
) -> (Platform, UnboundedReceiver<PlatformEvent>) {
    // Every hotkey in the settings is valid by now.
    let hotkeys = app::hotkeys(settings, false, false);
    let tray = Tray {
        tooltip: "Shuttercrab".into(),
        menu: app::tray_menu(settings, app::TrayRecording::Idle, None),
    };
    let (platform, events, conflicts) = match Platform::start(&hotkeys, Some(tray)) {
        Ok(started) => started,
        Err(e) => {
            log::error!("could not start the platform thread: {e:#}");
            std::process::exit(1);
        }
    };
    for conflict in &conflicts {
        log::warn!(
            "{} is taken by another application ({})",
            conflict.hotkey,
            conflict.reason
        );
        if conflict.id != QUIT_HOTKEY {
            notices.push((
                format!("{} is in use", conflict.hotkey),
                "Another app owns this hotkey. Click the Shuttercrab tray icon and capture from its window instead."
                    .to_string(),
            ));
        }
    }
    if first_run && conflicts.is_empty() {
        notices.push((
            "Shuttercrab is running".to_string(),
            format!(
                "Press {} to capture, or {} for an area.",
                settings.capture_bar_hotkey, settings.screenshot_hotkey
            ),
        ));
    }
    (platform, events)
}

/// A release build started from a terminal says where it went, then lets
/// the terminal go: the shell has already printed its prompt, and the log
/// file has the rest.
#[cfg(not(debug_assertions))]
fn release_terminal(log_file: Option<&Path>) {
    match log_file {
        Some(path) => eprintln!(
            "Shuttercrab is running in the tray. Log: {}",
            path.display()
        ),
        None => eprintln!("Shuttercrab is running in the tray."),
    }
    logging::stop_terminal();
    shuttercrab_platform::detach_from_terminal();
}
