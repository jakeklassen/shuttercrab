//! The tray icon's menu: its items' ids and what it offers.

use crate::settings::Settings;
use shuttercrab_platform::MenuItem;
use std::time::Duration;

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
pub const MENU_STREAM: u32 = 11;
pub const MENU_WATCH: u32 = 12;

/// A recording, or a live stream, as the tray menu shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayRecording {
    Idle,
    Running,
    Paused,
    Streaming,
}

/// A capture started from the tray menu waits this long, so the menu has
/// closed before the screen is frozen.
pub(super) const TRAY_DELAY: Duration = Duration::from_millis(250);

/// How long a hidden window takes to leave the screen where the OS cannot
/// leave it out of captures (X11): a few frames of the compositor.
pub(super) const FADE_OUT: Duration = Duration::from_millis(100);

/// The tray icon's context menu for the current settings and recording.
/// `update` is a downloaded release's version, offered as "Restart to
/// update" only while nothing is recording.
pub fn tray_menu(
    settings: &Settings,
    recording: TrayRecording,
    update: Option<&str>,
) -> Vec<MenuItem> {
    let record = match recording {
        TrayRecording::Idle | TrayRecording::Streaming => "Record an area",
        TrayRecording::Running | TrayRecording::Paused => "Stop recording",
    };
    let pause = match recording {
        TrayRecording::Idle | TrayRecording::Streaming => None,
        TrayRecording::Running => Some("Pause recording"),
        TrayRecording::Paused => Some("Resume recording"),
    };
    let stream = match recording {
        TrayRecording::Streaming => "Stop streaming",
        _ => "Stream an area…",
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
    .chain([
        MenuItem::item(MENU_STREAM, stream),
        MenuItem::item(MENU_WATCH, "Watch a stream…"),
    ])
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
            checked: Some(settings.auto_save),
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
                } => *checked,
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
        assert!(menu.contains(&MenuItem::item(MENU_STREAM, "Stream an area…")));
        assert!(menu.contains(&MenuItem::item(MENU_WATCH, "Watch a stream…")));
        let streaming = tray_menu(&settings, TrayRecording::Streaming, None);
        assert!(streaming.contains(&MenuItem::item(MENU_STREAM, "Stop streaming")));
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
        for recording in [
            TrayRecording::Running,
            TrayRecording::Paused,
            TrayRecording::Streaming,
        ] {
            assert!(!tray_menu(&settings, recording, Some("0.2.0")).contains(&offer));
        }
    }
}
