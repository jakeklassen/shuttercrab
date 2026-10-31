//! The platform service on Linux: it listens for requests from a second
//! start, shows the tray icon, grabs the hotkeys under X11, shows
//! notifications and copies.

use super::{
    clipboard,
    hotkey::{self, Hotkeys},
    instance::take_listener,
    launcher::icon_path,
    tray::TrayIcon,
};
use crate::{
    Hotkey, HotkeyConflict, MenuItem, PlatformEvent, Request, Tray, launcher::APP_ID, os::os,
};
use anyhow::{Result, anyhow};
use futures::channel::{mpsc, oneshot};
use shuttercrab_types::WindowId;
use std::{
    collections::HashMap,
    future::Future,
    io::{BufRead, BufReader},
    os::unix::net::UnixListener,
    path::PathBuf,
    sync::{Arc, OnceLock},
};
use zbus::{Connection, Variant};

pub use super::instance::signal_running_instance;

/// The platform service: a thread listening for requests from a second
/// start, the tray icon where the desktop shows one, and under X11 the
/// global hotkeys.
pub struct Platform {
    /// Keeps the event stream open.
    _events: mpsc::UnboundedSender<PlatformEvent>,
    hotkeys: Option<Hotkeys>,
    tray: Option<TrayIcon>,
}

impl Platform {
    /// Start listening for requests, show `tray` and, under X11, grab
    /// `hotkeys`. Returns those another app has.
    pub fn start(
        hotkeys: &[(u32, Hotkey)],
        tray: Option<Tray>,
    ) -> Result<(
        Self,
        mpsc::UnboundedReceiver<PlatformEvent>,
        Vec<HotkeyConflict>,
    )> {
        let (events, stream) = mpsc::unbounded();
        if let Some(listener) = take_listener() {
            let events = events.clone();
            std::thread::Builder::new()
                .name("shuttercrab-requests".into())
                .spawn(move || listen(&listener, &events))?;
        }
        let grabber = if hotkey::available() {
            Hotkeys::start(events.clone())
                .inspect_err(|e| log::warn!("no global hotkeys: {e:#}"))
                .ok()
        } else {
            None
        };
        let conflicts = grabber
            .as_ref()
            .map(|grabber| grabber.set(hotkeys.to_vec()))
            .unwrap_or_default();
        let tray = tray.filter(|_| os().tray).and_then(|tray| {
            TrayIcon::start(tray, events.clone())
                .inspect_err(|e| log::warn!("no tray icon: {e:#}"))
                .ok()
        });
        let platform = Self {
            _events: events,
            hotkeys: grabber,
            tray,
        };
        Ok((platform, stream, conflicts))
    }

    /// Copy a screenshot. arboard encodes its own PNG from `rgba`, so the
    /// one already made goes unused.
    pub fn copy_image(
        &self,
        _png: Arc<Vec<u8>>,
        rgba: Arc<Vec<u8>>,
        width: u32,
        height: u32,
    ) -> impl Future<Output = Result<()>> + use<> {
        on_thread(move || clipboard::copy_image(&rgba, width, height))
    }

    pub fn copy_files(&self, paths: Vec<PathBuf>) -> impl Future<Output = Result<()>> + use<> {
        on_thread(move || clipboard::copy_files(&paths))
    }

    /// Grab `hotkeys` in place of the ones grabbed now. A local X server
    /// answers within a round trip, so this is done before returning.
    pub fn set_hotkeys(
        &self,
        hotkeys: Vec<(u32, Hotkey)>,
    ) -> impl Future<Output = Vec<HotkeyConflict>> + use<> {
        let conflicts = self
            .hotkeys
            .as_ref()
            .map(|grabber| grabber.set(hotkeys))
            .unwrap_or_default();
        std::future::ready(conflicts)
    }

    pub fn set_tray_menu(&self, menu: Vec<MenuItem>) {
        if let Some(tray) = &self.tray {
            tray.set_menu(menu);
        }
    }

    pub fn watch_window(&self, _window: Option<WindowId>) {}

    /// A desktop notification, through the freedesktop notification
    /// service. Sent from a thread of their own, started with the first:
    /// they are D-Bus calls.
    pub fn notify(&self, title: impl Into<String>, message: impl Into<String>) {
        static NOTIFICATIONS: OnceLock<Option<std::sync::mpsc::Sender<(String, String)>>> =
            OnceLock::new();
        let notifications = NOTIFICATIONS.get_or_init(|| {
            let (send, notifications) = std::sync::mpsc::channel();
            let started = std::thread::Builder::new()
                .name("shuttercrab-notify".into())
                .spawn(move || send_notifications(&notifications));
            match started {
                Ok(_) => Some(send),
                Err(e) => {
                    log::warn!("could not start notifications: {e}");
                    None
                }
            }
        });
        if let Some(notifications) = notifications {
            let _ = notifications.send((title.into(), message.into()));
        }
    }
}

/// Run `work` on a thread of its own and hand back its result: X11 round
/// trips and PNG encoding stay off the UI thread.
fn on_thread(
    work: impl FnOnce() -> Result<()> + Send + 'static,
) -> impl Future<Output = Result<()>> {
    let (reply, answer) = oneshot::channel();
    let started = std::thread::Builder::new()
        .name("shuttercrab-clipboard".into())
        .spawn(move || {
            let _ = reply.send(work());
        });
    async move {
        started?;
        answer
            .await
            .unwrap_or_else(|_| Err(anyhow!("the clipboard thread stopped")))
    }
}

/// Send each notification (title, message) on Shuttercrab's session bus
/// connection, made at the first and kept open: GNOME takes back an app's
/// notifications as soon as the connection they came on closes. zbus runs
/// a connection on the thread that made it, so this thread keeps it.
fn send_notifications(notifications: &std::sync::mpsc::Receiver<(String, String)>) {
    let mut session = None;
    for (title, message) in notifications {
        if let Err(e) = zbus::block_on(send_notification(&mut session, &title, &message)) {
            log::warn!("could not show a notification: {e}");
        }
    }
}

/// Send one notification, on `session` once it is open. It is named by the
/// launcher entry, as the desktop finds the app's name and icon there.
async fn send_notification(
    session: &mut Option<Connection>,
    title: &str,
    message: &str,
) -> zbus::Result<()> {
    let connection = match session {
        Some(connection) => connection,
        None => session.insert(Connection::session().await?),
    };
    let icon = icon_path().map_or_else(String::new, |p| p.to_string_lossy().into_owned());
    let hints = HashMap::from([("desktop-entry", Variant::from(APP_ID))]);
    let actions: &[&str] = &[];
    // Notify(app name, replaces id, icon, summary, body, actions, hints,
    // timeout: the server's own).
    connection
        .call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &(
                "Shuttercrab",
                0u32,
                icon.as_str(),
                title,
                message,
                actions,
                hints,
                -1i32,
            ),
        )
        .await?;
    Ok(())
}

/// Turn each connection's first line into a request, until the socket
/// closes. A connection that sends nothing (a second start checking for
/// this one) asks for nothing.
fn listen(listener: &UnixListener, events: &mpsc::UnboundedSender<PlatformEvent>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let mut line = String::new();
        if BufReader::new(stream).read_line(&mut line).is_err() {
            continue;
        }
        let Some(request) = Request::from_name(line.trim()) else {
            continue;
        };
        if events
            .unbounded_send(PlatformEvent::AnotherInstance(request))
            .is_err()
        {
            return;
        }
    }
}
