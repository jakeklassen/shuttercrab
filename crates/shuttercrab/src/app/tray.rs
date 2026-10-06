//! The global hotkeys and the tray icon's menu: their ids, which hotkeys
//! apply when, and what the menu offers.

use crate::settings::Settings;
use shuttercrab_platform::{Hotkey, MenuItem};
use std::time::Duration;

/// Ids for the hotkeys the platform thread registers.
pub const SCREENSHOT_HOTKEY: u32 = 1;
pub const QUIT_HOTKEY: u32 = 2;
pub const CAPTURE_BAR_HOTKEY: u32 = 3;
pub const RECORD_HOTKEY: u32 = 4;
pub const PAUSE_HOTKEY: u32 = 5;
pub const RESTART_HOTKEY: u32 = 6;
pub const DISCARD_HOTKEY: u32 = 7;
pub const UNDO_HOTKEY: u32 = 8;
pub const SCREENSHOT_WITH_WINDOW_HOTKEY: u32 = 9;

/// While a hotkey field records, Print Screen with each mix of Ctrl, Alt
/// and Shift is registered under these ids, as windows never see that key
/// go down. The id's low bits say which: 1 Ctrl, 2 Alt, 4 Shift.
const PRINT_SCREEN_HOTKEYS: std::ops::Range<u32> = 16..24;

/// Print Screen, with every mix of Ctrl, Alt and Shift.
pub fn print_screen_hotkeys() -> Vec<(u32, Hotkey)> {
    PRINT_SCREEN_HOTKEYS
        .filter_map(|id| Some((id, print_screen_hotkey(id)?)))
        .collect()
}

/// The Print Screen hotkey registered under `id`, if it is one of those.
pub fn print_screen_hotkey(id: u32) -> Option<Hotkey> {
    PRINT_SCREEN_HOTKEYS.contains(&id).then(|| {
        let mix = id - PRINT_SCREEN_HOTKEYS.start;
        Hotkey {
            ctrl: mix & 1 != 0,
            alt: mix & 2 != 0,
            shift: mix & 4 != 0,
            win: false,
            key: VK_PRINT_SCREEN,
        }
    })
}

/// Print Screen's Win32 virtual-key code.
const VK_PRINT_SCREEN: u32 = 0x2C;

/// Ids for the tray menu's items.
pub const MENU_SCREENSHOT: u32 = 1;
pub const MENU_OPEN_FOLDER: u32 = 2;
pub const MENU_AUTO_SAVE: u32 = 3;
pub const MENU_QUIT: u32 = 4;
pub const MENU_CAPTURE_BAR: u32 = 5;
pub const MENU_SETTINGS: u32 = 6;
pub const MENU_RECORD: u32 = 7;
pub const MENU_PAUSE: u32 = 8;
pub const MENU_UPDATE: u32 = 9;
pub const MENU_OPEN: u32 = 10;

/// Quits without the tray menu; kept for development.
pub const QUIT_KEYS: &str = "Ctrl+Alt+Shift+Q";

/// The global hotkeys for `settings`: the Capture Bar, area screenshots
/// (without Shuttercrab's window and with it), recording, and quit; pause,
/// restart and discard while `recording`; and undo while there is
/// something to `undo`. Other apps keep those chords
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
        (
            SCREENSHOT_WITH_WINDOW_HOTKEY,
            settings.screenshot_with_window_hotkey.as_str(),
        ),
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
pub(super) const TRAY_DELAY: Duration = Duration::from_millis(250);

/// The tray icon's context menu for the current settings and recording.
/// `update` is a downloaded release's version, offered as "Restart to
/// update" only while nothing is recording.
pub fn tray_menu(
    settings: &Settings,
    recording: TrayRecording,
    update: Option<&str>,
) -> Vec<MenuItem> {
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
        // The window, as any tray app's menu offers first.
        MenuItem::item(MENU_OPEN, "Open Shuttercrab"),
        MenuItem::Separator,
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
    .chain(rest_of_tray_menu(
        settings,
        update.filter(|_| recording == TrayRecording::Idle),
    ))
    .collect()
}

/// The tray menu below the capture items.
fn rest_of_tray_menu(settings: &Settings, update: Option<&str>) -> Vec<MenuItem> {
    let update = update.map(|version| {
        [
            MenuItem::item(MENU_UPDATE, format!("Restart to update to {version}")),
            MenuItem::Separator,
        ]
    });
    let mut menu = vec![
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
    ];
    menu.extend(update.into_iter().flatten());
    menu.push(MenuItem::item(MENU_QUIT, "Quit Shuttercrab"));
    menu
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
        let menu = tray_menu(&settings, TrayRecording::Idle, None);
        assert!(auto_save_checked(&menu));
        // The window comes first, as in any tray app.
        assert_eq!(menu[0], MenuItem::item(MENU_OPEN, "Open Shuttercrab"));
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
        let running = tray_menu(&settings, TrayRecording::Running, None);
        assert!(running.contains(&MenuItem::item(MENU_RECORD, "Stop recording\tCtrl+Alt+R")));
        assert_eq!(
            pause_item(&running),
            Some(MenuItem::item(MENU_PAUSE, "Pause recording\tCtrl+Alt+P"))
        );
        assert_eq!(
            pause_item(&tray_menu(&settings, TrayRecording::Paused, None)),
            Some(MenuItem::item(MENU_PAUSE, "Resume recording\tCtrl+Alt+P"))
        );
        let off = Settings {
            auto_save: false,
            ..settings
        };
        assert!(!auto_save_checked(&tray_menu(
            &off,
            TrayRecording::Idle,
            None
        )));
    }

    #[test]
    fn a_downloaded_update_is_offered_only_while_nothing_records() {
        let settings = Settings::default();
        let offer = MenuItem::item(MENU_UPDATE, "Restart to update to 0.2.0");
        assert!(!tray_menu(&settings, TrayRecording::Idle, None).contains(&offer));
        let idle = tray_menu(&settings, TrayRecording::Idle, Some("0.2.0"));
        // Just above Quit, with a separator between.
        let at = idle
            .iter()
            .position(|item| *item == offer)
            .expect("offered");
        assert_eq!(idle[at + 1], MenuItem::Separator);
        assert!(matches!(idle[at + 2], MenuItem::Item { id: MENU_QUIT, .. }));
        for recording in [TrayRecording::Running, TrayRecording::Paused] {
            assert!(!tray_menu(&settings, recording, Some("0.2.0")).contains(&offer));
        }
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
        assert!(idle.contains(&SCREENSHOT_WITH_WINDOW_HOTKEY));
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
}
