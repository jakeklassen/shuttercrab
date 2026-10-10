//! The platform service on Linux. So far it listens for requests from a
//! second start and shows notifications; the tray, hotkeys and clipboard
//! come later.

use super::instance::take_listener;
use crate::{Hotkey, HotkeyConflict, MenuItem, PlatformEvent, Request, Tray};
use anyhow::{Result, anyhow};
use futures::channel::mpsc;
use shuttercrab_types::WindowId;
use std::{
    future::Future,
    io::{BufRead, BufReader},
    os::unix::net::UnixListener,
    path::PathBuf,
    sync::Arc,
};

pub use super::instance::signal_running_instance;

/// The platform service: a thread listening for requests from a second
/// start.
pub struct Platform {
    /// Keeps the event stream open.
    _events: mpsc::UnboundedSender<PlatformEvent>,
}

impl Platform {
    /// Start listening for requests. There are no global hotkeys on Linux
    /// yet, so none are registered or reported as taken.
    pub fn start(
        _hotkeys: &[(u32, Hotkey)],
        _tray: Option<Tray>,
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
        Ok((Self { _events: events }, stream, Vec::new()))
    }

    pub fn copy_image(
        &self,
        _png: Arc<Vec<u8>>,
        _rgba: Arc<Vec<u8>>,
        _width: u32,
        _height: u32,
    ) -> impl Future<Output = Result<()>> + use<> {
        std::future::ready(Err(anyhow!("copying images is not supported on Linux yet")))
    }

    pub fn copy_files(&self, _paths: Vec<PathBuf>) -> impl Future<Output = Result<()>> + use<> {
        std::future::ready(Err(anyhow!("copying files is not supported on Linux yet")))
    }

    pub fn set_hotkeys(
        &self,
        _hotkeys: Vec<(u32, Hotkey)>,
    ) -> impl Future<Output = Vec<HotkeyConflict>> + use<> {
        std::future::ready(Vec::new())
    }

    pub fn set_tray_menu(&self, _menu: Vec<MenuItem>) {}

    pub fn watch_window(&self, _window: Option<WindowId>) {}

    /// A desktop notification, through the freedesktop notification
    /// service. Sent from a thread of its own: it is a D-Bus call.
    pub fn notify(&self, title: impl Into<String>, message: impl Into<String>) {
        let (title, message) = (title.into(), message.into());
        let sent = std::thread::Builder::new()
            .name("shuttercrab-notify".into())
            .spawn(move || {
                let shown = notify_rust::Notification::new()
                    .appname("Shuttercrab")
                    .summary(&title)
                    .body(&message)
                    .show();
                if let Err(e) = shown {
                    log::warn!("could not show a notification: {e}");
                }
            });
        if let Err(e) = sent {
            log::warn!("could not start a notification: {e}");
        }
    }
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
