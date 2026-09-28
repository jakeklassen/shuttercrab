// Release builds are a windowed app on Windows, with no console behind them.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use framecut::app::{QUIT_HOTKEY, SCREENSHOT_HOTKEY};
use framecut_capture::Capture;
use framecut_platform::{Hotkey, Platform};

/// Win+Shift+S belongs to Windows and PrintScreen to Snipping Tool; these
/// are free by default. Settings arrive in Milestone 2.
const SCREENSHOT: &str = "Ctrl+Alt+S";
const QUIT: &str = "Ctrl+Alt+Shift+Q";

fn main() {
    // Release builds have no console; print to the terminal that started us.
    #[cfg(not(debug_assertions))]
    framecut_platform::attach_to_parent_terminal();

    let capture = match Capture::start() {
        Ok(capture) => capture,
        Err(e) => {
            eprintln!("framecut: {e}");
            std::process::exit(1);
        }
    };
    capture.warm_up();
    let hotkeys = [
        (
            SCREENSHOT_HOTKEY,
            Hotkey::parse(SCREENSHOT).expect("valid default hotkey"),
        ),
        (
            QUIT_HOTKEY,
            Hotkey::parse(QUIT).expect("valid default hotkey"),
        ),
    ];
    let (platform, events, conflicts) = match Platform::start(&hotkeys) {
        Ok(started) => started,
        Err(e) => {
            eprintln!("framecut: {e:#}");
            std::process::exit(1);
        }
    };
    for conflict in &conflicts {
        eprintln!(
            "framecut: {} is taken by another application ({})",
            conflict.hotkey, conflict.reason
        );
    }
    if conflicts.iter().any(|c| c.id == SCREENSHOT_HOTKEY) {
        eprintln!("framecut: is Framecut already running?");
        std::process::exit(2);
    }
    eprintln!("framecut: press {SCREENSHOT} to take a screenshot, {QUIT} to quit");

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        framecut::app::run(capture, platform, events, cx);
    });
}
