//! The main window, with its Settings page, and the hooks it uses to reach
//! the rest of Shuttercrab.

use super::{
    Busy, Start, State, quit, restart_to_update, start,
    tray::{TRAY_DELAY, hotkeys},
};
use crate::{
    capture_bar::{CaptureMode, CaptureTarget},
    main_window::{self, MainHooks, MainWindow, Page},
    popup,
    settings_window::{Diagnostics, Hooks},
};
use gpui_kit::{
    App, AppContext as _, AsyncApp, Bounds, TitlebarOptions, Window, WindowBounds, WindowKind,
    WindowOptions, component::Root,
};
use shuttercrab_capture::MonitorInfo;
use shuttercrab_platform::window as platform_window;
use std::{cell::RefCell, rc::Rc, time::Duration};

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
    let open = state.main_window.borrow().clone();
    if let Some((window, view)) = open
        && window
            .update(cx, |_, window, cx| {
                view.update(cx, |view, cx| view.show(page, window, cx));
                // Restored if minimised, and brought forward; but left hidden
                // while a capture it started is being taken.
                if state.hidden_main.get().is_none()
                    && let Some(hwnd) = popup::raw_hwnd(window)
                {
                    platform_window::show_normal(hwnd);
                }
            })
            .is_ok()
    {
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
                focus: true,
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
                    view.show(page, window, cx);
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
                    platform_window::show_normal(hwnd);
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
    MainHooks {
        settings: Rc::new(settings_hooks(state, monitors)),
        capture: Rc::new(move |mode, target, window, cx| {
            start_from_main(&capture, mode, target, window, cx)
        }),
        open_folder: Rc::new(move |cx| {
            let state = folder.clone();
            cx.spawn(async move |cx| open_folder(&state, cx)).detach();
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
    }
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
