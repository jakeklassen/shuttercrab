//! The tray icon on Linux: a StatusNotifierItem on the session bus, with
//! its menu over dbusmenu, through ksni. GNOME shows these with its
//! AppIndicator extension (on in Ubuntu); KDE and most other desktops
//! show them as they are.

use crate::{Hotkey, MenuItem, PlatformEvent, Tray, icon, launcher::APP_ID};
use anyhow::Result;
use futures::channel::mpsc::UnboundedSender;
use ksni::blocking::{Handle, TrayMethods};

/// Whether the desktop shows tray icons: something owns the
/// StatusNotifierWatcher name on the session bus.
pub(super) fn available() -> bool {
    let has_owner = || -> zbus::Result<bool> {
        let bus = zbus::blocking::Connection::session()?;
        let proxy = zbus::blocking::fdo::DBusProxy::new(&bus)?;
        Ok(proxy.name_has_owner("org.kde.StatusNotifierWatcher".try_into()?)?)
    };
    has_owner().unwrap_or(false)
}

/// The icon on the desktop's tray, kept while Shuttercrab runs.
pub(super) struct TrayIcon {
    handle: Handle<Item>,
}

impl TrayIcon {
    /// Show `tray`; clicks and menu choices go to `events`.
    pub(super) fn start(tray: Tray, events: UnboundedSender<PlatformEvent>) -> Result<Self> {
        let item = Item {
            tooltip: tray.tooltip,
            menu: tray.menu,
            icons: [16, 22, 24, 32, 48, 64]
                .into_iter()
                .map(argb_icon)
                .collect(),
            events,
        };
        Ok(Self {
            handle: item.spawn()?,
        })
    }

    pub(super) fn set_menu(&self, menu: Vec<MenuItem>) {
        self.handle.update(|item| item.menu = menu);
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        // Taken off the tray; nothing waits for it.
        drop(self.handle.shutdown());
    }
}

struct Item {
    tooltip: String,
    menu: Vec<MenuItem>,
    icons: Vec<ksni::Icon>,
    events: UnboundedSender<PlatformEvent>,
}

impl ksni::Tray for Item {
    fn id(&self) -> String {
        APP_ID.into()
    }

    fn title(&self) -> String {
        "Shuttercrab".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.tooltip.clone(),
            ..Default::default()
        }
    }

    /// A click on the icon, where the desktop sends one rather than
    /// opening the menu.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.events.unbounded_send(PlatformEvent::TrayActivated);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        self.menu.iter().map(menu_item).collect()
    }
}

/// One entry of the menu. A tab in a label sets off its hotkey, which goes
/// to the menu as the item's shortcut. A checkmark item stays one when
/// unticked: GNOME's AppIndicator keeps a tick an item stops sending.
fn menu_item(item: &MenuItem) -> ksni::MenuItem<Item> {
    let MenuItem::Item {
        id,
        label,
        enabled,
        checked,
    } = item
    else {
        return ksni::MenuItem::Separator;
    };
    let (text, hotkey) = label.split_once('\t').unwrap_or((label, ""));
    // Underscores mark access keys in dbusmenu; doubled, they are shown.
    let label = text.replace('_', "__");
    let shortcut = shortcut(hotkey);
    let id = *id;
    let activate = Box::new(move |item: &mut Item| {
        let _ = item.events.unbounded_send(PlatformEvent::TrayCommand(id));
    });
    if let Some(checked) = *checked {
        ksni::menu::CheckmarkItem {
            label,
            enabled: *enabled,
            checked,
            shortcut,
            activate,
            ..Default::default()
        }
        .into()
    } else {
        ksni::menu::StandardItem {
            label,
            enabled: *enabled,
            shortcut,
            activate,
            ..Default::default()
        }
        .into()
    }
}

/// A hotkey as dbusmenu spells a shortcut: `[["Control", "Alt", "C"]]`.
fn shortcut(hotkey: &str) -> Vec<Vec<String>> {
    let Ok(hotkey) = Hotkey::parse(hotkey) else {
        return Vec::new();
    };
    let text = hotkey.to_string();
    let key = text.rsplit('+').next().unwrap_or_default();
    let keys = [
        (hotkey.ctrl, "Control"),
        (hotkey.alt, "Alt"),
        (hotkey.shift, "Shift"),
        (hotkey.win, "Super"),
    ]
    .into_iter()
    .filter(|&(on, _)| on)
    .map(|(_, name)| name)
    .chain([key])
    .map(String::from)
    .collect();
    vec![keys]
}

/// The icon at `size`, as ARGB in network byte order.
fn argb_icon(size: u32) -> ksni::Icon {
    let data = icon::rgba(size)
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, a]| [a, r, g, b])
        .collect();
    ksni::Icon {
        width: size as i32,
        height: size as i32,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkeys_become_shortcuts() {
        assert_eq!(
            shortcut("Ctrl+Alt+Shift+S"),
            [["Control", "Alt", "Shift", "S"]]
        );
        assert_eq!(shortcut("Win+F12"), [["Super", "F12"]]);
        assert!(shortcut("").is_empty());
    }

    #[test]
    fn the_icon_is_argb() {
        let icon = argb_icon(16);
        assert_eq!(icon.data.len(), 16 * 16 * 4);
        let rgba = icon::rgba(16);
        assert_eq!(&icon.data[..4], &[rgba[3], rgba[0], rgba[1], rgba[2]]);
    }
}
