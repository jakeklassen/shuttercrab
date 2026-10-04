//! The main window, with its Settings page, and the hooks it uses to reach
//! the rest of Shuttercrab.

use super::{
    Busy, Start, State, quit, restart_to_update, start,
    tray::{TRAY_DELAY, hotkeys},
};
use crate::{
    capture_choice::{CaptureMode, CaptureTarget},
    files,
    main_window::{self, MainHooks, MainWindow, Page, Shot},
    pixels, popup,
    settings_window::{Diagnostics, Hooks},
};
use gpui_kit::{
    App, AppContext as _, AsyncApp, Bounds, Task, TitlebarOptions, Window, WindowBounds,
    WindowKind, WindowOptions, component::Root,
};
use shuttercrab_capture::MonitorInfo;
use shuttercrab_platform::window as platform_window;
use std::{cell::RefCell, path::Path, rc::Rc, sync::Arc, time::Duration};

/// The most of the screen's width and height the main window takes for a
/// big screenshot, which is then scaled down to fit. Like Snipping Tool,
/// the window stays a comfortable size however big the screenshot.
const MAX_SCREEN_SHARE: f32 = 0.5;

pub(super) fn open_folder(state: &State, cx: &mut AsyncApp) {
    let dir = state.settings.borrow().output_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::error!("could not create {}: {e}", dir.display());
        state.notify("Could not open the folder", e.to_string(), None);
        return;
    }
    cx.update(|cx| cx.open_with_system(&dir));
}

/// Open the main window on `page`, or bring it forward on `page` if it is
/// open. Settings is a page of it, so Shuttercrab has one window.
pub(super) fn open_main(state: &Rc<State>, page: Page, cx: &mut AsyncApp) {
    open_window(state, page, None, Focus::Take, cx);
}

/// Show `shot` in the main window, opening it if needed, as Snipping Tool
/// shows a new snip.
pub(super) fn show_in_main(state: &Rc<State>, shot: Shot, focus: Focus, cx: &mut AsyncApp) {
    open_window(state, Page::Home, Some(shot), focus, cx);
}

/// Whether the main window takes the keyboard when it comes forward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Focus {
    Take,
    /// It stays with the active window: after a screenshot taken there,
    /// to paste it.
    Leave,
}

/// Bring the window forward, with the keyboard or without.
fn bring_forward(hwnd: isize, focus: Focus) {
    match focus {
        Focus::Take => platform_window::show_normal(hwnd),
        Focus::Leave => platform_window::show_without_focus(hwnd),
    }
}

/// Open the main window, or bring it forward, on `page` or showing `shot`.
fn open_window(
    state: &Rc<State>,
    page: Page,
    mut shot: Option<Shot>,
    focus: Focus,
    cx: &mut AsyncApp,
) {
    let open = state.main_window.borrow().clone();
    if let Some((window, view)) = open
        && let Ok(hwnd) = window.update(cx, |_, window, cx| {
            view.update(cx, |view, cx| match shot.take() {
                Some(shot) => view.show_shot(shot, window, cx),
                None => view.show(page, window, cx),
            });
            popup::raw_hwnd(window)
        })
    {
        // Restored if minimised, and brought forward; but left hidden while
        // a capture it started is being taken. In a task of its own: after
        // this update, which the window's activation would interrupt, and
        // after the window is sized for a screenshot.
        if state.hidden_main.get().is_none()
            && let Some(hwnd) = hwnd
        {
            cx.spawn(async move |_| bring_forward(hwnd, focus)).detach();
        }
        return;
    }
    let state = state.clone();
    cx.spawn(async move |cx| {
        let monitors = state.capture.list_monitors().await.unwrap_or_default();
        let hooks = Rc::new(main_hooks(&state, monitors));
        let resume = hooks.settings.apply_hotkeys.clone();
        let closing = state.clone();
        // The view, out of the window's builder.
        let out = Rc::new(RefCell::new(None));
        let slot = out.clone();
        let size = match page {
            Page::Home => main_window::HOME_SIZE,
            Page::Settings => main_window::SETTINGS_SIZE,
        };
        let opened = cx.update(|cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Shuttercrab".into()),
                    ..Default::default()
                }),
                focus: focus == Focus::Take,
                show: true,
                kind: WindowKind::Normal,
                ..Default::default()
            };
            cx.open_window(options, move |window, cx| {
                main_window::apply_theme(window, cx);
                // The title bar's close button: re-register the hotkeys
                // (closing while a new one is being typed must not leave them
                // paused), and close; Shuttercrab stays in the tray. GPUI's
                // own close logs errors as the window goes.
                window.on_window_should_close(cx, move |window, cx| {
                    drop(resume());
                    closing.main_window.replace(None);
                    popup::close_window(window, cx);
                    false
                });
                let view = cx.new(|cx| {
                    let mut view = MainWindow::new(hooks, window, cx);
                    match shot {
                        Some(shot) => view.show_shot(shot, window, cx),
                        None => view.show(page, window, cx),
                    }
                    view
                });
                slot.replace(Some(view.clone()));
                cx.new(|cx| Root::new(view, window, cx))
            })
        });
        match opened.map(|window| (window, out.borrow_mut().take())) {
            Ok((window, Some(view))) => {
                log::info!("main window opened");
                let hwnd = window
                    .update(cx, |_, window, _| popup::raw_hwnd(window))
                    .ok()
                    .flatten();
                if let Some(hwnd) = hwnd {
                    bring_forward(hwnd, focus);
                }
                state.main_window.replace(Some((window.into(), view)));
            }
            Ok((_, None)) => log::error!("the main window opened without its view"),
            Err(e) => log::error!("could not open the main window: {e:#}"),
        }
    })
    .detach();
}

/// The main window's way to start a capture: hide the window, wait for it
/// to be gone (and for the screenshot delay), then capture. The window comes
/// back when the capture, or the recording, is over.
fn start_from_main(
    state: &Rc<State>,
    mode: CaptureMode,
    target: CaptureTarget,
    window: &Window,
    cx: &mut App,
) {
    if state.busy.get() != Busy::Idle {
        log::info!("{mode:?} ignored: a capture is already open");
        return;
    }
    if let Some(hwnd) = popup::raw_hwnd(window) {
        platform_window::hide(hwnd);
        state.hidden_main.set(Some(hwnd));
    }
    let (what, wait) = match mode {
        CaptureMode::Screenshot => (
            Start::Screenshot(target),
            Duration::from_secs(state.settings.borrow().delay().into()),
        ),
        // Recordings have their own countdown, over the chosen area.
        CaptureMode::Record => (Start::Record(target), Duration::ZERO),
    };
    let state = state.clone();
    cx.spawn(async move |cx| start(&state, what, Some(TRAY_DELAY + wait), cx))
        .detach();
}

/// How the main window reaches the rest of Shuttercrab.
fn main_hooks(state: &Rc<State>, monitors: Vec<MonitorInfo>) -> MainHooks {
    let (capture, folder, quitting) = (state.clone(), state.clone(), state.clone());
    let (ready, restarting) = (state.clone(), state.clone());
    let (copying, saving) = (state.clone(), state.clone());
    let (painting, opening) = (state.clone(), state.clone());
    MainHooks {
        settings: Rc::new(settings_hooks(state, monitors)),
        capture: Rc::new(move |mode, target, window, cx| {
            start_from_main(&capture, mode, target, window, cx)
        }),
        open_folder: Rc::new(move |shot, cx| match shot.and_then(|s| s.saved.as_ref()) {
            Some(path) => cx.reveal_path(path),
            None => {
                let state = folder.clone();
                cx.spawn(async move |cx| open_folder(&state, cx)).detach();
            }
        }),
        quit: Rc::new(move |cx| {
            let state = quitting.clone();
            cx.spawn(async move |cx| quit(&state, cx).await).detach();
        }),
        update_ready: Rc::new(move || ready.update_ready.borrow().clone()),
        restart_to_update: Rc::new(move |cx| {
            let state = restarting.clone();
            cx.spawn(async move |cx| restart_to_update(&state, cx).await)
                .detach();
        }),
        copy: Rc::new(move |shot, cx| copy_shot(&copying, shot, cx)),
        save_as: Rc::new(move |shot, cx| save_shot_as(&saving, shot, cx)),
        edit_in_paint: Rc::new(move |shot, cx| {
            open_shot_file(&painting, shot, cx, |path| {
                shuttercrab_platform::open::edit_in_paint(path).map_err(|e| {
                    log::error!("{e:#}");
                    "Paint could not be started. Is it installed?"
                })
            })
        }),
        open_with: Rc::new(move |shot, cx| {
            open_shot_file(&opening, shot, cx, |path| {
                shuttercrab_platform::open::open_with(path);
                Ok(())
            })
        }),
        fit_window: Rc::new(|window, cx, width, height| {
            let Some(hwnd) = popup::raw_hwnd(window) else {
                return;
            };
            // After this update, as GPUI resizes its own windows: Windows
            // reports the new size to GPUI at once, and GPUI is still busy
            // updating this window now.
            cx.foreground_executor()
                .spawn(async move {
                    let fitted =
                        platform_window::fit_client_area(hwnd, width, height, MAX_SCREEN_SHARE);
                    if let Err(e) = fitted {
                        log::warn!("could not size the window: {e:#}");
                    }
                })
                .detach();
        }),
    }
}

/// Copy the screenshot the main window shows to the clipboard.
/// Resolves to whether it was copied.
fn copy_shot(state: &Rc<State>, shot: &Shot, cx: &mut App) -> Task<bool> {
    let (state, png, image) = (state.clone(), shot.png.clone(), shot.image.clone());
    let (width, height) = shot.size();
    cx.spawn(async move |cx| {
        let rgba = cx
            .background_executor()
            .spawn(async move { pixels::rgba(&image) })
            .await;
        let copied = state
            .platform
            .copy_image(png, Arc::new(rgba), width, height)
            .await;
        match copied {
            Ok(()) => {
                log::info!("{width}×{height} screenshot copied from the window");
                true
            }
            Err(e) => {
                log::error!("copy: {e:#}");
                state.notify(
                    "Could not copy the screenshot",
                    "Another app is holding the clipboard. Try again in a moment.",
                    None,
                );
                false
            }
        }
    })
}

/// Hand the screenshot the main window shows to another program, through
/// `open`: its file in the screenshots folder, or, if it was not saved
/// there, one written to the temporary folder for it. `open` may answer
/// with a message for the user.
fn open_shot_file(
    state: &Rc<State>,
    shot: &Shot,
    cx: &mut App,
    open: impl FnOnce(&Path) -> Result<(), &'static str> + 'static,
) {
    let (state, saved, png, taken_at) = (
        state.clone(),
        shot.saved.clone(),
        shot.png.clone(),
        shot.taken_at,
    );
    cx.spawn(async move |cx| {
        let path = match saved {
            Some(path) => Ok(path),
            None => {
                cx.background_executor()
                    .spawn(
                        async move { files::save_screenshot(&files::temp_dir(), taken_at, &png) },
                    )
                    .await
            }
        };
        let opened = match path {
            Ok(path) => open(&path),
            Err(e) => {
                log::error!("could not write the screenshot: {e:#}");
                Err("The screenshot could not be written to a file.")
            }
        };
        if let Err(message) = opened {
            state.notify("Could not open the screenshot", message, None);
        }
    })
    .detach();
}

/// Ask where to save the screenshot the main window shows, starting in the
/// screenshots folder with its usual name, and save it there.
fn save_shot_as(state: &Rc<State>, shot: &Shot, cx: &mut App) {
    let dir = state.settings.borrow().output_dir();
    // The dialog opens in the folder only if it exists.
    let _ = std::fs::create_dir_all(&dir);
    let chosen = cx.prompt_for_new_path(&dir, Some(&files::capture_name(shot.taken_at)));
    let (state, png) = (state.clone(), shot.png.clone());
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(mut path))) = chosen.await else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension("png");
        }
        let target = path.clone();
        let written = cx
            .background_executor()
            .spawn(async move { std::fs::write(&target, &*png) })
            .await;
        match written {
            Ok(()) => log::info!("saved the screenshot as {}", path.display()),
            Err(e) => {
                log::error!("could not save {}: {e}", path.display());
                state.notify(
                    "Could not save the screenshot",
                    format!("{} could not be written: {e}", path.display()),
                    None,
                );
            }
        }
    })
    .detach();
}

/// How the settings window reaches the rest of Shuttercrab.
fn settings_hooks(state: &Rc<State>, monitors: Vec<MonitorInfo>) -> Hooks {
    let (changed, pause, apply, probe) =
        (state.clone(), state.clone(), state.clone(), state.clone());
    Hooks {
        settings: state.settings.clone(),
        changed: Rc::new(move |cx: &mut App| {
            let settings = changed.settings.borrow().clone();
            changed.save(&settings, cx.background_executor());
            changed.refresh_tray_menu();
            log::info!("settings changed");
        }),
        pause_hotkeys: Rc::new(move || {
            // The request is sent at once; nothing waits for the answer.
            drop(pause.platform.set_hotkeys(Vec::new()));
        }),
        apply_hotkeys: Rc::new(move || {
            let registering = apply.platform.set_hotkeys(apply.hotkeys());
            Box::pin(async move {
                registering
                    .await
                    .into_iter()
                    .map(|conflict| {
                        log::warn!("{} is taken by another application", conflict.hotkey);
                        conflict.id
                    })
                    .collect()
            })
        }),
        probe_hotkeys: Rc::new(move || {
            let every = hotkeys(&probe.settings.borrow(), true, true);
            let registering = probe.platform.set_hotkeys(every);
            let state = probe.clone();
            Box::pin(async move {
                let taken = registering
                    .await
                    .into_iter()
                    .map(|conflict| {
                        log::warn!("{} is taken by another application", conflict.hotkey);
                        conflict.id
                    })
                    .collect();
                // Back to what applies now; any clash there is in `taken`.
                drop(state.platform.set_hotkeys(state.hotkeys()).await);
                taken
            })
        }),
        launch_at_startup: Rc::new(shuttercrab_platform::startup::launch_at_startup),
        set_launch_at_startup: Rc::new(|enabled| {
            match shuttercrab_platform::startup::set_launch_at_startup(enabled) {
                Ok(()) => log::info!("launch at startup {}", if enabled { "on" } else { "off" }),
                Err(e) => log::error!("{e:#}"),
            }
        }),
        diagnostics: Diagnostics {
            version: env!("CARGO_PKG_VERSION").to_string(),
            windows_build: shuttercrab_capture::display::windows_build(),
            monitors,
            log_dir: state.log_dir.clone(),
            settings_path: state.settings_path.clone(),
        },
    }
}
