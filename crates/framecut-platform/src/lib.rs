//! Framecut's Windows integration that GPUI does not provide: global
//! hotkeys, the clipboard, and window behaviour applied to a GPUI window's
//! `HWND` (PRD §17, §18). UI code calls this crate; it never touches Win32
//! itself.
#![cfg(windows)]

mod clipboard;
mod console;
mod hotkey;
pub mod window;

pub use clipboard::dibv5;
pub use console::attach_to_parent_terminal;
pub use hotkey::Hotkey;

use anyhow::{Context, Result, anyhow};
use futures::channel::{mpsc, oneshot};
use std::sync::mpsc::{Receiver, Sender, channel};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
                HWND_MESSAGE, MSG, PostMessageW, PostQuitMessage, RegisterClassW, WINDOW_EX_STYLE,
                WINDOW_STYLE, WM_APP, WM_HOTKEY, WNDCLASSW,
            },
        },
    },
    core::w,
};

/// Something the platform thread observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformEvent {
    /// A registered hotkey was pressed; the id is the caller's.
    Hotkey(u32),
}

/// A hotkey that could not be registered, usually because another
/// application already owns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotkeyConflict {
    pub id: u32,
    pub hotkey: Hotkey,
    pub reason: String,
}

enum Command {
    CopyImage {
        png: Vec<u8>,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        reply: oneshot::Sender<Result<()>>,
    },
}

/// Posted to the platform window to make its thread drain `commands`.
const WM_COMMANDS: u32 = WM_APP + 1;
/// Posted to stop the thread.
const WM_STOP: u32 = WM_APP + 2;

/// The platform thread. Dropping it unregisters the hotkeys and stops the
/// thread.
pub struct Platform {
    commands: Sender<Command>,
    window: isize,
}

impl Platform {
    /// Start the platform thread and register `hotkeys`. Returns the handle,
    /// the stream of events, and any hotkeys that could not be registered.
    pub fn start(
        hotkeys: &[(u32, Hotkey)],
    ) -> Result<(
        Self,
        mpsc::UnboundedReceiver<PlatformEvent>,
        Vec<HotkeyConflict>,
    )> {
        let (events, stream) = mpsc::unbounded();
        let (commands, inbox) = channel();
        let (ready, started) = channel::<Result<(isize, Vec<HotkeyConflict>)>>();
        let hotkeys = hotkeys.to_vec();
        std::thread::Builder::new()
            .name("framecut-platform".into())
            .spawn(move || run(hotkeys, events, inbox, ready))
            .context("could not start the platform thread")?;
        let (window, conflicts) = started.recv().context("the platform thread stopped")??;
        Ok((Self { commands, window }, stream, conflicts))
    }

    /// Put an image on the clipboard as PNG and as a bitmap. Retries briefly
    /// if another application holds the clipboard.
    pub fn copy_image(
        &self,
        png: Vec<u8>,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
    ) -> impl Future<Output = Result<()>> + use<> {
        let (reply, answer) = oneshot::channel();
        let sent = self
            .commands
            .send(Command::CopyImage {
                png,
                rgba,
                width,
                height,
                reply,
            })
            .is_ok()
            && unsafe {
                PostMessageW(
                    Some(HWND(self.window as _)),
                    WM_COMMANDS,
                    WPARAM(0),
                    LPARAM(0),
                )
            }
            .is_ok();
        async move {
            if !sent {
                return Err(anyhow!("the platform thread stopped"));
            }
            answer
                .await
                .unwrap_or_else(|_| Err(anyhow!("the platform thread stopped")))
        }
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        let _ =
            unsafe { PostMessageW(Some(HWND(self.window as _)), WM_STOP, WPARAM(0), LPARAM(0)) };
    }
}

extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn run(
    hotkeys: Vec<(u32, Hotkey)>,
    events: mpsc::UnboundedSender<PlatformEvent>,
    inbox: Receiver<Command>,
    ready: Sender<Result<(isize, Vec<HotkeyConflict>)>>,
) {
    let window = match create_message_window() {
        Ok(window) => window,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut conflicts = Vec::new();
    for (id, hotkey) in &hotkeys {
        if let Err(e) =
            unsafe { RegisterHotKey(Some(window), *id as i32, hotkey.modifiers(), hotkey.key) }
        {
            conflicts.push(HotkeyConflict {
                id: *id,
                hotkey: *hotkey,
                reason: e.message(),
            });
        }
    }
    let _ = ready.send(Ok((window.0 as isize, conflicts)));

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        match msg.message {
            WM_HOTKEY => {
                let _ = events.unbounded_send(PlatformEvent::Hotkey(msg.wParam.0 as u32));
            }
            WM_COMMANDS => {
                while let Ok(command) = inbox.try_recv() {
                    match command {
                        Command::CopyImage {
                            png,
                            rgba,
                            width,
                            height,
                            reply,
                        } => {
                            let _ =
                                reply.send(clipboard::write(window, &png, &rgba, width, height));
                        }
                    }
                }
            }
            WM_STOP => unsafe { PostQuitMessage(0) },
            _ => unsafe {
                DispatchMessageW(&msg);
            },
        }
    }
    for (id, _) in &hotkeys {
        let _ = unsafe { UnregisterHotKey(Some(window), *id as i32) };
    }
    let _ = unsafe { DestroyWindow(window) };
}

fn create_message_window() -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: w!("FramecutPlatform"),
            ..Default::default()
        };
        // Registering twice (a second Platform in one process) fails
        // harmlessly; creating the window below is what matters.
        RegisterClassW(&class);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("FramecutPlatform"),
            w!("Framecut"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
        .context("could not create the platform message window")
    }
}
