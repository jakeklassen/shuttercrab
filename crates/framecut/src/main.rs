// Release builds are a windowed app on Windows, with no console behind them.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use framecut::{
    app::{self, Framecut, QUIT_HOTKEY, SCREENSHOT_HOTKEY},
    logging,
    settings::{self, Loaded, Settings},
};
use framecut_capture::{Capture, display::windows_build};
use framecut_platform::{Hotkey, Platform, Tray};
use std::path::PathBuf;

/// Quits without the tray menu; kept for development.
const QUIT: &str = "Ctrl+Alt+Shift+Q";

/// Keeps settings and logs in this folder instead of the user's, for tests.
const DATA_DIR_VARIABLE: &str = "FRAMECUT_DATA_DIR";

fn main() {
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

    let screenshot = match Hotkey::parse(&settings.screenshot_hotkey) {
        Ok(hotkey) => hotkey,
        Err(e) => {
            let fallback = Settings::default().screenshot_hotkey;
            log::error!(
                "screenshot hotkey {:?} is not valid ({e:#}); using {fallback}",
                settings.screenshot_hotkey
            );
            notices.push((
                "Hotkey not recognised".to_string(),
                format!(
                    "\"{}\" is not a valid hotkey, so {fallback} is used instead.",
                    settings.screenshot_hotkey
                ),
            ));
            Hotkey::parse(&fallback).expect("the default hotkey is valid")
        }
    };
    // Show the hotkey as Framecut writes it, whatever the file's spelling.
    settings.screenshot_hotkey = screenshot.to_string();

    let capture = match Capture::start() {
        Ok(capture) => capture,
        Err(e) => {
            log::error!("could not start capture: {e:#}");
            std::process::exit(1);
        }
    };
    capture.warm_up();

    let hotkeys = [
        (SCREENSHOT_HOTKEY, screenshot),
        (
            QUIT_HOTKEY,
            Hotkey::parse(QUIT).expect("the quit hotkey is valid"),
        ),
    ];
    let tray = Tray {
        tooltip: "Framecut".into(),
        menu: app::tray_menu(&settings),
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
        if conflict.id == SCREENSHOT_HOTKEY {
            notices.push((
                format!("{} is in use", conflict.hotkey),
                "Another app owns this hotkey. Take screenshots from the Framecut tray icon."
                    .to_string(),
            ));
        }
    }
    if first_run && conflicts.is_empty() {
        notices.push((
            "Framecut is running".to_string(),
            format!(
                "Press {} to take a screenshot. Framecut lives in the tray.",
                settings.screenshot_hotkey
            ),
        ));
    }
    // One at a time: Windows shows only the latest tray notification.
    if let Some((title, message)) = notices.into_iter().next() {
        platform.notify(title, message);
    }
    log::info!(
        "ready: {} takes a screenshot, {QUIT} quits",
        settings.screenshot_hotkey
    );

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        app::run(
            Framecut {
                capture,
                platform,
                settings,
                settings_path,
            },
            events,
            cx,
        );
    });
}
