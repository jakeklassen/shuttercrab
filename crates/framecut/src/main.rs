// Release builds are a windowed app on Windows, with no console behind them.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use framecut::{
    app::{self, Framecut, QUIT_HOTKEY},
    logging,
    settings::{self, Loaded, Settings},
};
use framecut_capture::{Capture, display::windows_build};
use framecut_platform::{Hotkey, Platform, Tray};
use std::path::PathBuf;

/// Keeps settings and logs in this folder instead of the user's, for tests.
const DATA_DIR_VARIABLE: &str = "FRAMECUT_DATA_DIR";

/// Counts the Rust heap for the memory lines in the log.
#[global_allocator]
static HEAP: framecut::heap::Counting = framecut::heap::Counting;

fn main() {
    // The recording helper: no tray, no UI, no single-instance check.
    if std::env::args().nth(1).as_deref() == Some(framecut::recorder_process::FLAG) {
        std::process::exit(framecut::recorder_process::serve());
    }
    // Release builds have no console; print to the terminal that started us.
    #[cfg(not(debug_assertions))]
    framecut_platform::attach_to_parent_terminal();

    let Some(_instance) = framecut_platform::single_instance("Framecut") else {
        // The log file belongs to the running instance; leave it alone.
        eprintln!("Framecut is already running.");
        framecut_platform::signal_running_instance();
        return;
    };
    let data_dir = std::env::var_os(DATA_DIR_VARIABLE).map(PathBuf::from);
    let log_dir = match &data_dir {
        Some(dir) => Some(dir.join("logs")),
        None => logging::default_dir(),
    };
    let log_file = logging::init(log_dir.as_deref());
    log::info!(
        "Framecut {} on Windows build {}",
        env!("CARGO_PKG_VERSION"),
        windows_build()
    );
    match &log_file {
        Some(path) => log::info!("logging to {}", path.display()),
        None => log::warn!("no log file; logging to the terminal only"),
    }

    let settings_path = match &data_dir {
        Some(dir) => Some(dir.join("settings.json")),
        None => settings::default_path(),
    };
    let mut notices = Vec::new();
    let (mut settings, first_run) = match settings_path.as_deref().map(settings::load) {
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
    if let Some(path) = &settings_path {
        log::info!("settings in {}", path.display());
    }
    // Screenshots written only for the thumbnail to open or drag.
    let removed = framecut::files::remove_old(
        &framecut::files::temp_dir(),
        std::time::Duration::from_secs(24 * 60 * 60),
    );
    if removed > 0 {
        log::info!("removed {removed} temporary screenshots older than a day");
    }

    let defaults = Settings::default();
    // Keep valid hotkeys in Framecut's spelling; replace invalid ones.
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

    let capture = match Capture::start() {
        Ok(capture) => capture,
        Err(e) => {
            log::error!("could not start capture: {e:#}");
            std::process::exit(1);
        }
    };
    capture.warm_up();

    // Every hotkey in the settings is valid now.
    let hotkeys = app::hotkeys(&settings, false, false);
    let tray = Tray {
        tooltip: "Framecut".into(),
        menu: app::tray_menu(&settings, app::TrayRecording::Idle),
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
                "Another app owns this hotkey. Click the Framecut tray icon to capture instead."
                    .to_string(),
            ));
        }
    }
    if first_run && conflicts.is_empty() {
        notices.push((
            "Framecut is running".to_string(),
            format!(
                "Press {} to capture, or {} for an area. Framecut lives in the tray.",
                settings.capture_bar_hotkey, settings.screenshot_hotkey
            ),
        ));
    }
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

    gpui_kit::application()
        .with_assets(framecut::icons::Icons)
        .run(move |cx| {
            gpui_kit::init(cx);
            app::run(
                Framecut {
                    capture,
                    platform,
                    settings,
                    settings_path,
                    log_dir: log_file.as_ref().and_then(|f| f.parent().map(Into::into)),
                },
                events,
                cx,
            );
        });
}
