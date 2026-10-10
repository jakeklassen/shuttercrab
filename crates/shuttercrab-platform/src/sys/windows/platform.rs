//! The platform thread on Windows: a hidden window whose message loop
//! owns the tray icon, the hotkeys, the clipboard and the window watching.

use super::{clipboard, icon, watch};
use crate::{Hotkey, HotkeyConflict, MenuItem, PlatformEvent, Tray};
use anyhow::{Context, Result, anyhow};
use futures::channel::{mpsc, oneshot};
use std::{
    cell::RefCell,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{Receiver, Sender, channel},
    },
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
            Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey},
            Shell::{
                NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_NOSOUND, NIM_ADD,
                NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_BALLOONUSERCLICK, NIN_SELECT,
                NOTIFY_ICON_DATA_FLAGS, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
                DestroyMenu, DestroyWindow, DispatchMessageW, FindWindowW, GetCursorPos,
                GetMessageW, GetSystemMetrics, HICON, MF_CHECKED, MF_GRAYED, MF_SEPARATOR,
                MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
                RegisterWindowMessageW, SM_CXSMICON, SetForegroundWindow, TPM_NONOTIFY,
                TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WINDOW_STYLE, WM_APP,
                WM_CONTEXTMENU, WM_DISPLAYCHANGE, WM_HOTKEY, WM_NULL, WNDCLASSW, WS_EX_TOOLWINDOW,
            },
        },
    },
    core::{HSTRING, PCWSTR, w},
};
enum Command {
    CopyImage {
        png: Arc<Vec<u8>>,
        rgba: Arc<Vec<u8>>,
        width: u32,
        height: u32,
        reply: oneshot::Sender<Result<()>>,
    },
    CopyFiles {
        paths: Vec<PathBuf>,
        reply: oneshot::Sender<Result<()>>,
    },
    SetHotkeys(Vec<(u32, Hotkey)>, oneshot::Sender<Vec<HotkeyConflict>>),
    SetMenu(Vec<MenuItem>),
    Notify {
        title: String,
        message: String,
    },
    Watch(Option<isize>),
}

/// Posted to the platform window to make its thread drain `commands`.
const WM_COMMANDS: u32 = WM_APP + 1;
/// Posted to stop the thread.
const WM_STOP: u32 = WM_APP + 2;
/// The tray icon's callback message.
const WM_TRAY: u32 = WM_APP + 3;
/// Posted by a second Shuttercrab process to the running one.
const WM_ANOTHER_INSTANCE: u32 = WM_APP + 4;
const TRAY_ID: u32 = 1;
/// `NIN_SELECT | NINF_KEY` from shellapi.h: the icon was chosen with the
/// keyboard. windows 0.62 does not define it.
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

/// The platform thread. Dropping it removes the tray icon, unregisters the
/// hotkeys and stops the thread.
pub struct Platform {
    commands: Sender<Command>,
    window: isize,
}

impl Platform {
    /// Start the platform thread, register `hotkeys` and, with `tray`, add
    /// the tray icon. Returns the handle, the stream of events, and any
    /// hotkeys that could not be registered.
    pub fn start(
        hotkeys: &[(u32, Hotkey)],
        tray: Option<Tray>,
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
            .name("shuttercrab-platform".into())
            .spawn(move || run(hotkeys, tray, events, inbox, ready))
            .context("could not start the platform thread")?;
        let (window, conflicts) = started.recv().context("the platform thread stopped")??;
        Ok((Self { commands, window }, stream, conflicts))
    }

    fn send(&self, command: Command) -> bool {
        self.commands.send(command).is_ok()
            && unsafe {
                PostMessageW(
                    Some(HWND(self.window as _)),
                    WM_COMMANDS,
                    WPARAM(0),
                    LPARAM(0),
                )
            }
            .is_ok()
    }

    /// Put an image on the clipboard as PNG and as a bitmap. Retries briefly
    /// if another application holds the clipboard. The buffers are shared,
    /// not copied.
    pub fn copy_image(
        &self,
        png: Arc<Vec<u8>>,
        rgba: Arc<Vec<u8>>,
        width: u32,
        height: u32,
    ) -> impl Future<Output = Result<()>> + use<> {
        let (reply, answer) = oneshot::channel();
        let sent = self.send(Command::CopyImage {
            png,
            rgba,
            width,
            height,
            reply,
        });
        async move {
            if !sent {
                return Err(anyhow!("the platform thread stopped"));
            }
            answer
                .await
                .unwrap_or_else(|_| Err(anyhow!("the platform thread stopped")))
        }
    }

    /// Put files on the clipboard, as Explorer's Copy does. Retries briefly
    /// if another application holds the clipboard.
    pub fn copy_files(&self, paths: Vec<PathBuf>) -> impl Future<Output = Result<()>> + use<> {
        let (reply, answer) = oneshot::channel();
        let sent = self.send(Command::CopyFiles { paths, reply });
        async move {
            if !sent {
                return Err(anyhow!("the platform thread stopped"));
            }
            answer
                .await
                .unwrap_or_else(|_| Err(anyhow!("the platform thread stopped")))
        }
    }

    /// Replace the registered hotkeys. Resolves to the ones that could not be
    /// registered.
    pub fn set_hotkeys(
        &self,
        hotkeys: Vec<(u32, Hotkey)>,
    ) -> impl Future<Output = Vec<HotkeyConflict>> + use<> {
        let (reply, answer) = oneshot::channel();
        let sent = self.send(Command::SetHotkeys(hotkeys, reply));
        async move {
            if !sent {
                return Vec::new();
            }
            answer.await.unwrap_or_default()
        }
    }

    /// Replace the tray icon's context menu.
    pub fn set_tray_menu(&self, menu: Vec<MenuItem>) {
        self.send(Command::SetMenu(menu));
    }

    /// Report when `window` (an `HWND`) moves, changes size, or is minimised
    /// or restored, as [`PlatformEvent::Window`], in place of the window
    /// watched before; `None` stops watching.
    pub fn watch_window(&self, window: Option<isize>) {
        self.send(Command::Watch(window));
    }

    /// Show a notification from the tray icon (a toast on Windows 11).
    pub fn notify(&self, title: impl Into<String>, message: impl Into<String>) {
        self.send(Command::Notify {
            title: title.into(),
            message: message.into(),
        });
    }
}

/// Tell the running Shuttercrab that another copy was started, so it can say
/// it is already running. Returns whether one was found.
pub fn signal_running_instance() -> bool {
    unsafe {
        FindWindowW(w!("ShuttercrabPlatform"), w!("Shuttercrab")).is_ok_and(|window| {
            PostMessageW(Some(window), WM_ANOTHER_INSTANCE, WPARAM(0), LPARAM(0)).is_ok()
        })
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        let _ =
            unsafe { PostMessageW(Some(HWND(self.window as _)), WM_STOP, WPARAM(0), LPARAM(0)) };
    }
}

/// What the window procedure needs; it lives on the platform thread only.
pub(super) struct State {
    window: HWND,
    pub(super) events: mpsc::UnboundedSender<PlatformEvent>,
    tray: Option<Tray>,
    icon: Option<HICON>,
    taskbar_created: u32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

pub(super) fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.borrow_mut().as_mut().map(f))
}

extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_TRAY {
        // NOTIFYICON_VERSION_4: the event is in the low word of lparam.
        let event = (lparam.0 & 0xFFFF) as u32;
        match event {
            NIN_SELECT | NIN_KEYSELECT => {
                with_state(|s| s.events.unbounded_send(PlatformEvent::TrayActivated));
            }
            WM_CONTEXTMENU => show_menu(hwnd),
            NIN_BALLOONUSERCLICK => {
                with_state(|s| s.events.unbounded_send(PlatformEvent::NotificationClicked));
            }
            _ => {}
        }
        return LRESULT(0);
    }
    // Sent to every top-level window, this hidden one included; Windows
    // still needs its default handling.
    if message == WM_DISPLAYCHANGE {
        with_state(|s| s.events.unbounded_send(PlatformEvent::DisplaysChanged));
    }
    // Explorer restarted: the tray icon is gone and must be added again.
    if with_state(|s| s.taskbar_created != 0 && message == s.taskbar_created) == Some(true) {
        with_state(add_tray_icon);
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn show_menu(hwnd: HWND) {
    let Some(items) = with_state(|s| s.tray.as_ref().map(|t| t.menu.clone()).unwrap_or_default())
    else {
        return;
    };
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        for item in &items {
            let _ = match item {
                MenuItem::Separator => AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()),
                MenuItem::Item {
                    id,
                    label,
                    enabled,
                    checked,
                } => {
                    let mut flags = MF_STRING;
                    if !*enabled {
                        flags |= MF_GRAYED;
                    }
                    if *checked {
                        flags |= MF_CHECKED;
                    }
                    AppendMenuW(menu, flags, *id as usize, &HSTRING::from(label.as_str()))
                }
            };
        }
        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        // Without the foreground, the menu would not close when the user
        // clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            at.x,
            at.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        if chosen.0 != 0 {
            with_state(|s| {
                s.events
                    .unbounded_send(PlatformEvent::TrayCommand(chosen.0 as u32))
            });
        }
    }
}

fn copy_utf16<const N: usize>(target: &mut [u16; N], text: &str) {
    let units: Vec<u16> = text.encode_utf16().take(N - 1).collect();
    target[..units.len()].copy_from_slice(&units);
    target[units.len()] = 0;
}

fn tray_data(s: &State, flags: NOTIFY_ICON_DATA_FLAGS) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: s.window,
        uID: TRAY_ID,
        uFlags: flags,
        uCallbackMessage: WM_TRAY,
        hIcon: s.icon.unwrap_or_default(),
        ..Default::default()
    };
    if let Some(tray) = &s.tray {
        copy_utf16(&mut data.szTip, &tray.tooltip);
    }
    data
}

fn add_tray_icon(s: &mut State) {
    if s.tray.is_none() {
        return;
    }
    if s.icon.is_none() {
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as u32;
        s.icon = icon::hicon(size).ok();
    }
    let mut data = tray_data(s, NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP);
    unsafe {
        if Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
            log::debug!("tray icon added");
        } else {
            log::warn!("could not add the tray icon");
        }
    }
}

fn notify(s: &State, title: &str, message: &str) {
    if s.tray.is_none() {
        return;
    }
    let mut data = tray_data(s, NIF_INFO);
    copy_utf16(&mut data.szInfoTitle, title);
    copy_utf16(&mut data.szInfo, message);
    data.dwInfoFlags = NIIF_NOSOUND;
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

fn register_hotkeys(window: HWND, hotkeys: &[(u32, Hotkey)]) -> Vec<HotkeyConflict> {
    let mut conflicts = Vec::new();
    for (id, hotkey) in hotkeys {
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
    conflicts
}

fn unregister_hotkeys(window: HWND, hotkeys: &[(u32, Hotkey)]) {
    for (id, _) in hotkeys {
        let _ = unsafe { UnregisterHotKey(Some(window), *id as i32) };
    }
}

fn run(
    hotkeys: Vec<(u32, Hotkey)>,
    tray: Option<Tray>,
    events: mpsc::UnboundedSender<PlatformEvent>,
    inbox: Receiver<Command>,
    ready: Sender<Result<(isize, Vec<HotkeyConflict>)>>,
) {
    // Tray icon sizes and menu positions are physical pixels.
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let window = match create_window() {
        Ok(window) => window,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut hotkeys = hotkeys;
    let conflicts = register_hotkeys(window, &hotkeys);
    let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            window,
            events: events.clone(),
            tray,
            icon: None,
            taskbar_created,
        })
    });
    with_state(add_tray_icon);
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
                        Command::CopyFiles { paths, reply } => {
                            let _ = reply.send(clipboard::write_files(window, &paths));
                        }
                        Command::SetHotkeys(new, reply) => {
                            unregister_hotkeys(window, &hotkeys);
                            let _ = reply.send(register_hotkeys(window, &new));
                            hotkeys = new;
                        }
                        Command::SetMenu(menu) => {
                            with_state(|s| {
                                if let Some(tray) = &mut s.tray {
                                    tray.menu = menu;
                                }
                            });
                        }
                        Command::Notify { title, message } => {
                            with_state(|s| notify(s, &title, &message));
                        }
                        Command::Watch(window) => watch::watch(window),
                    }
                }
            }
            WM_ANOTHER_INSTANCE => {
                let _ = events.unbounded_send(PlatformEvent::AnotherInstance);
            }
            WM_STOP => unsafe { PostQuitMessage(0) },
            _ => unsafe {
                DispatchMessageW(&msg);
            },
        }
    }
    unregister_hotkeys(window, &hotkeys);
    with_state(|s| {
        if s.tray.is_some() {
            let data = tray_data(s, NOTIFY_ICON_DATA_FLAGS(0));
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            }
        }
        if let Some(icon) = s.icon.take() {
            unsafe {
                let _ = DestroyIcon(icon);
            }
        }
    });
    STATE.with(|s| s.borrow_mut().take());
    let _ = unsafe { DestroyWindow(window) };
}

/// A hidden top-level window: message-only windows cannot own a tray icon
/// that survives an Explorer restart, because they miss the
/// `TaskbarCreated` broadcast.
fn create_window() -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: w!("ShuttercrabPlatform"),
            ..Default::default()
        };
        // Registering twice (a second Platform in one process) fails
        // harmlessly; creating the window below is what matters.
        RegisterClassW(&class);
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("ShuttercrabPlatform"),
            w!("Shuttercrab"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .context("could not create the platform window")
    }
}
