//! Shuttercrab's OS integration that GPUI does not provide: global
//! hotkeys, the tray icon, notifications, the clipboard, and window
//! behaviour applied to a GPUI window's native handle (PRD §17, §18). UI
//! code calls this crate; it never touches the OS itself.
//!
//! Each public module is the same on every OS. What only an OS can do lives
//! in `sys`, one backend per OS; Windows is the only one so far, and other
//! OSes get one that says they are not supported yet.

pub mod cursor;
pub mod drag;
pub mod frame;
mod hotkey;
pub mod icon;
pub mod memory;
pub mod ocr;
pub mod open;
pub mod os;
pub mod process;
pub mod startup;
mod sys;
pub mod targets;
pub mod window;

pub use hotkey::{Hotkey, Key, print_screen_taken};
#[cfg(windows)]
pub use sys::imp::clipboard::dibv5;
pub use sys::imp::{
    console::{attach_to_parent_terminal, detach_from_terminal},
    instance::{SingleInstance, single_instance},
    platform::{Platform, signal_running_instance},
    watch::visible_bounds,
};

/// Something the platform thread observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformEvent {
    /// A registered hotkey was pressed; the id is the caller's.
    Hotkey(u32),
    /// The tray icon was clicked (or chosen with the keyboard).
    TrayActivated,
    /// A tray menu item was chosen; the id is the caller's.
    TrayCommand(u32),
    /// Shuttercrab was started again while this instance was running.
    AnotherInstance,
    /// The user clicked the latest notification.
    NotificationClicked,
    /// A display was attached, detached or changed.
    DisplaysChanged,
    /// The window being watched ([`Platform::watch_window`]) changed.
    Window(WindowChange),
}

/// What happened to the watched window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowChange {
    /// It moved or changed size: its visible bounds now, physical pixels.
    Moved(frame::Rect),
    /// Dragging it to move or resize it began.
    DragStarted,
    /// The drag ended.
    DragEnded,
    Minimized,
    /// Hidden or cloaked without being minimised: some apps do this when
    /// closed to the tray, or closed while they keep running.
    Hidden,
    /// Shown again after being minimised or hidden.
    Restored,
    /// It no longer exists.
    Closed,
}

/// A hotkey that could not be registered, usually because another
/// application already owns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotkeyConflict {
    pub id: u32,
    pub hotkey: Hotkey,
    pub reason: String,
}

/// One entry of the tray icon's context menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuItem {
    Item {
        id: u32,
        label: String,
        enabled: bool,
        /// Shows a checkmark.
        checked: bool,
    },
    Separator,
}

impl MenuItem {
    pub fn item(id: u32, label: impl Into<String>) -> Self {
        MenuItem::Item {
            id,
            label: label.into(),
            enabled: true,
            checked: false,
        }
    }
}

/// The tray icon: its tooltip and context menu.
#[derive(Clone, Debug, Default)]
pub struct Tray {
    pub tooltip: String,
    pub menu: Vec<MenuItem>,
}
