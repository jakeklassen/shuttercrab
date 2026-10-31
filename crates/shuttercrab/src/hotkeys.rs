//! The app's global hotkeys: which there are, where the settings keep each
//! one's chord, the id the platform thread registers it under, and when it
//! is registered. The Settings window adds its own words for each.

use crate::settings::Settings;
use shuttercrab_platform::{Hotkey, Key};

/// A global hotkey the user can change in Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyKind {
    CaptureBar,
    Screenshot,
    ScreenshotWithWindow,
    Record,
    Pause,
    Restart,
    Discard,
    Undo,
}

/// When a hotkey is registered. Other apps keep its chord the rest of the
/// time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Registered {
    Always,
    WhileRecording,
    /// While a discard can be undone, or a restart's previous take kept.
    WhileUndoable,
}

impl HotkeyKind {
    /// Every hotkey, in the order Settings shows them.
    pub const ALL: [HotkeyKind; 8] = [
        Self::CaptureBar,
        Self::Screenshot,
        Self::ScreenshotWithWindow,
        Self::Record,
        Self::Pause,
        Self::Restart,
        Self::Discard,
        Self::Undo,
    ];

    /// The id the platform thread registers it under. Ids last only while
    /// Shuttercrab runs; [`QUIT_HOTKEY`] and the Print Screen ids are
    /// different.
    pub const fn hotkey_id(self) -> u32 {
        match self {
            Self::Screenshot => 1,
            Self::CaptureBar => 3,
            Self::Record => 4,
            Self::Pause => 5,
            Self::Restart => 6,
            Self::Discard => 7,
            Self::Undo => 8,
            Self::ScreenshotWithWindow => 9,
        }
    }

    /// The hotkey registered under `id`, if it is one of these.
    pub fn from_hotkey_id(id: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.hotkey_id() == id)
    }

    /// Its chord in `settings`, such as `Ctrl+Alt+S`.
    pub fn get(self, settings: &Settings) -> &str {
        match self {
            Self::CaptureBar => &settings.capture_bar_hotkey,
            Self::Screenshot => &settings.screenshot_hotkey,
            Self::ScreenshotWithWindow => &settings.screenshot_with_window_hotkey,
            Self::Record => &settings.record_hotkey,
            Self::Pause => &settings.pause_hotkey,
            Self::Restart => &settings.restart_hotkey,
            Self::Discard => &settings.discard_hotkey,
            Self::Undo => &settings.undo_hotkey,
        }
    }

    /// Change its chord in `settings`.
    pub fn set(self, settings: &mut Settings, chord: String) {
        let slot = match self {
            Self::CaptureBar => &mut settings.capture_bar_hotkey,
            Self::Screenshot => &mut settings.screenshot_hotkey,
            Self::ScreenshotWithWindow => &mut settings.screenshot_with_window_hotkey,
            Self::Record => &mut settings.record_hotkey,
            Self::Pause => &mut settings.pause_hotkey,
            Self::Restart => &mut settings.restart_hotkey,
            Self::Discard => &mut settings.discard_hotkey,
            Self::Undo => &mut settings.undo_hotkey,
        };
        *slot = chord;
    }

    /// When it is registered.
    pub fn registered(self) -> Registered {
        match self {
            Self::CaptureBar | Self::Screenshot | Self::ScreenshotWithWindow | Self::Record => {
                Registered::Always
            }
            Self::Pause | Self::Restart | Self::Discard => Registered::WhileRecording,
            Self::Undo => Registered::WhileUndoable,
        }
    }
}

/// The id of the hotkey that quits without the tray menu, kept for
/// development. It is not in Settings.
pub const QUIT_HOTKEY: u32 = 2;
pub const QUIT_KEYS: &str = "Ctrl+Alt+Shift+Q";

/// The global hotkeys to register for `settings`, with their ids: each
/// [`HotkeyKind`] whose time it is, while `recording` and while there is
/// something to `undo`, and quit. A chord the settings spell wrongly is
/// left out (main checks them at startup; the Settings window only stores
/// valid ones).
pub fn hotkeys(settings: &Settings, recording: bool, undo: bool) -> Vec<(u32, Hotkey)> {
    let now = |kind: &HotkeyKind| match kind.registered() {
        Registered::Always => true,
        Registered::WhileRecording => recording,
        Registered::WhileUndoable => undo,
    };
    HotkeyKind::ALL
        .iter()
        .filter(|kind| now(kind))
        .map(|kind| (kind.hotkey_id(), kind.get(settings)))
        .chain([(QUIT_HOTKEY, QUIT_KEYS)])
        .filter_map(|(id, chord)| Some((id, Hotkey::parse(chord).ok()?)))
        .collect()
}

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
            key: Key::PrintScreen,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hotkey_has_an_id_of_its_own() {
        let mut ids: Vec<u32> = HotkeyKind::ALL.iter().map(|k| k.hotkey_id()).collect();
        ids.push(QUIT_HOTKEY);
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), HotkeyKind::ALL.len() + 1);
        assert!(ids.iter().all(|id| !PRINT_SCREEN_HOTKEYS.contains(id)));
        for kind in HotkeyKind::ALL {
            assert_eq!(HotkeyKind::from_hotkey_id(kind.hotkey_id()), Some(kind));
        }
    }

    #[test]
    fn each_hotkey_reads_and_writes_its_own_setting() {
        let mut settings = Settings::default();
        for (i, kind) in HotkeyKind::ALL.into_iter().enumerate() {
            kind.set(&mut settings, format!("Ctrl+F{}", i + 1));
        }
        for (i, kind) in HotkeyKind::ALL.into_iter().enumerate() {
            assert_eq!(kind.get(&settings), format!("Ctrl+F{}", i + 1));
        }
    }

    #[test]
    fn recording_chords_are_taken_only_while_they_mean_something() {
        use HotkeyKind::*;
        let settings = Settings::default();
        let ids = |recording, undo| {
            hotkeys(&settings, recording, undo)
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>()
        };
        let during = [Pause, Restart, Discard].map(HotkeyKind::hotkey_id);
        let undo = Undo.hotkey_id();
        let idle = ids(false, false);
        assert!(idle.contains(&Record.hotkey_id()));
        assert!(idle.contains(&ScreenshotWithWindow.hotkey_id()));
        assert!(idle.contains(&QUIT_HOTKEY));
        assert!(during.iter().all(|id| !idle.contains(id)));
        assert!(!idle.contains(&undo));
        let recording = ids(true, false);
        assert!(during.iter().all(|id| recording.contains(id)));
        assert!(!recording.contains(&undo));
        // Undo also outlives the recording: a restart's previous take can
        // be kept after Stop.
        assert!(ids(true, true).contains(&undo));
        assert!(ids(false, true).contains(&undo));
    }
}
