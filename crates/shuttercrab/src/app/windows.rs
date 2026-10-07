//! The main window, with its Settings page, and the hooks it uses to reach
//! the rest of Shuttercrab.

use super::{
    Busy, Start, State, quit, restart_to_update, start,
    tray::{TRAY_DELAY, hotkeys, print_screen_hotkeys},
};
use crate::{
    capture_choice::{CaptureMode, CaptureTarget},
    files,
    main_window::{self, MainHooks, MainWindow, Page, Place, Shot},
    markup, pixels, playback, popup,
    settings_window::{Diagnostics, Hooks},
};
use gpui_kit::{
    App, AppContext as _, AsyncApp, Bounds, Context, Task, TitlebarOptions, Window, WindowBounds,
    WindowKind, WindowOptions, component::Root,
};
use shuttercrab_capture::MonitorInfo;
use shuttercrab_platform::window as platform_window;
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

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
    open_window(state, page, None, cx);
}

/// Show `shot` in the main window, opening it if needed, as Snipping Tool
/// shows a new snip.
pub(super) fn show_in_main(state: &Rc<State>, shot: Shot, cx: &mut AsyncApp) {
    open_window(state, Page::Home, Some(Content::Shot(shot)), cx);
}

/// Show the recording at `path`, `size` physical pixels, in the main
/// window, opening it if needed, ready to play.
pub(super) fn show_recording_in_main(
    state: &Rc<State>,
    path: PathBuf,
    size: (u32, u32),
    cx: &mut AsyncApp,
) {
    open_window(state, Page::Home, Some(Content::Recording(path, size)), cx);
}

/// What the main window opens showing, other than a page.
enum Content {
    Shot(Shot),
    Recording(PathBuf, (u32, u32)),
}

/// Show `content` in the main window, or `page` without it.
fn show_content(
    view: &mut MainWindow,
    page: Page,
    content: Option<Content>,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
) {
    match content {
        Some(Content::Shot(shot)) => view.show_shot(shot, window, cx),
        Some(Content::Recording(path, size)) => view.show_recording(path, Some(size), window, cx),
        None => view.show(page, window, cx),
    }
}

/// Open the main window, or bring it forward with the keyboard, on `page`
/// or showing `content`.
fn open_window(state: &Rc<State>, page: Page, mut content: Option<Content>, cx: &mut AsyncApp) {
    let open = state.main_window.borrow().clone();
    if let Some((window, view)) = open
        && let Ok(hwnd) = window.update(cx, |_, window, cx| {
            view.update(cx, |view, cx| {
                show_content(view, page, content.take(), window, cx)
            });
            popup::raw_hwnd(window)
        })
    {
        // Restored if minimised, and brought forward; but left hidden while
        // a capture is being taken. In a task of its own: after
        // this update, which the window's activation would interrupt, and
        // after the window is sized for a screenshot.
        if state.hidden_main.get().is_none()
            && let Some(hwnd) = hwnd
        {
            cx.spawn(async move |_| platform_window::show_normal(hwnd))
                .detach();
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
                focus: true,
                show: true,
                kind: WindowKind::Normal,
                // No narrower than the narrow toolbar, so nothing is cut off.
                window_min_size: Some(main_window::MIN_SIZE),
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
                    show_content(&mut view, page, content, window, cx);
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
    let (copying, saving) = (state.clone(), state.clone());
    let (painting, opening) = (state.clone(), state.clone());
    let (copying_file, saving_file) = (state.clone(), state.clone());
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
        window_place: Rc::new(|window| {
            let hwnd = popup::raw_hwnd(window)?;
            let (x, y, width, height) = platform_window::outer_bounds(hwnd)
                .inspect_err(|e| log::warn!("could not read where the window is: {e:#}"))
                .ok()?;
            Some(Place {
                x,
                y,
                width,
                height,
            })
        }),
        restore_place: Rc::new(|window, cx, place| {
            let Some(hwnd) = popup::raw_hwnd(window) else {
                return;
            };
            // After this update, as for fitting.
            cx.foreground_executor()
                .spawn(async move {
                    let bounds = (place.x, place.y, place.width, place.height);
                    if let Err(e) = platform_window::place(hwnd, bounds) {
                        log::warn!("could not put the window back: {e:#}");
                    }
                })
                .detach();
        }),
        open_recording: Rc::new(playback::open),
        reveal: Rc::new(|path, cx| cx.reveal_path(path)),
        copy_file: Rc::new(move |path, cx| copy_recording(&copying_file, path, cx)),
        save_file_as: Rc::new(move |path, cx| save_recording_as(&saving_file, path, cx)),
        open_file_with: Rc::new(|path, _| shuttercrab_platform::open::open_with(path)),
        fit_window: Rc::new(|window, cx, fit| {
            let Some(hwnd) = popup::raw_hwnd(window) else {
                return;
            };
            // After this update, as GPUI resizes its own windows: Windows
            // reports the new size to GPUI at once, and GPUI is still busy
            // updating this window now.
            cx.foreground_executor()
                .spawn(async move {
                    let fitted = platform_window::fit_client_area(
                        hwnd,
                        (fit.width, fit.height),
                        MAX_SCREEN_SHARE,
                        (fit.least_width, fit.least_height),
                    );
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
    let (state, shot) = (state.clone(), shot.clone());
    let (width, height) = shot.output_size();
    cx.spawn(async move |cx| {
        let flattened = cx
            .background_executor()
            .spawn(async move { flatten(&shot) })
            .await;
        let copied = match flattened {
            Ok((rgba, png)) => {
                state
                    .platform
                    .copy_image(png, Arc::new(rgba), width, height)
                    .await
            }
            Err(e) => Err(e),
        };
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

/// Copy the recording the main window shows to the clipboard as a file,
/// as Explorer does: it pastes into a folder or a chat as the MP4.
fn copy_recording(state: &Rc<State>, path: &Path, cx: &mut App) -> Task<bool> {
    let (state, path) = (state.clone(), path.to_path_buf());
    cx.spawn(
        async move |_| match state.platform.copy_files(vec![path.clone()]).await {
            Ok(()) => {
                log::info!(
                    "{} copied from the window",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
                true
            }
            Err(e) => {
                log::error!("copy: {e:#}");
                state.notify(
                    "Could not copy the recording",
                    "Another app is holding the clipboard. Try again in a moment.",
                    None,
                );
                false
            }
        },
    )
}

/// Ask where to save a copy of the recording the main window shows,
/// starting beside it with its name, and copy it there.
fn save_recording_as(state: &Rc<State>, path: &Path, cx: &mut App) {
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    let chosen = cx.prompt_for_new_path(&dir, name.as_deref());
    let (state, source) = (state.clone(), path.to_path_buf());
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(mut target))) = chosen.await else {
            return;
        };
        if target.extension().is_none() {
            target.set_extension("mp4");
        }
        // Saved over itself: it is already there.
        if target == source {
            return;
        }
        let to = target.clone();
        let copied = cx
            .background_executor()
            .spawn(async move { std::fs::copy(&source, &to) })
            .await;
        match copied {
            Ok(_) => log::info!("saved the recording as {}", target.display()),
            Err(e) => {
                log::error!("could not save {}: {e}", target.display());
                state.notify(
                    "Could not save the recording",
                    format!("{} could not be written: {e}", target.display()),
                    None,
                );
            }
        }
    })
    .detach();
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
    // A marked-up or cropped screenshot is never the saved file, which is
    // as taken.
    let saved = shot.saved.clone().filter(|_| !shot.is_edited());
    let (state, shot) = (state.clone(), shot.clone());
    cx.spawn(async move |cx| {
        let path = match saved {
            Some(path) => Ok(path),
            None => {
                cx.background_executor()
                    .spawn(async move {
                        let (_, png) = flatten(&shot)?;
                        files::save_screenshot(&files::temp_dir(), shot.taken_at, &png)
                    })
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
    let (state, shot) = (state.clone(), shot.clone());
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
            .spawn(async move {
                let (_, png) = flatten(&shot)?;
                anyhow::Ok(std::fs::write(&target, &*png)?)
            })
            .await;
        match written {
            Ok(()) => log::info!("saved the screenshot as {}", path.display()),
            Err(e) => {
                log::error!("could not save {}: {e:#}", path.display());
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

/// The screenshot's pixels (straight-alpha RGBA) and its PNG file, with
/// its marks drawn on and cut to its crop. Unedited, the PNG is the one
/// taken.
fn flatten(shot: &Shot) -> anyhow::Result<(Vec<u8>, Arc<Vec<u8>>)> {
    let rgba = pixels::rgba(&shot.image);
    if !shot.is_edited() {
        return Ok((rgba, shot.png.clone()));
    }
    let (width, height) = shot.size();
    let kept = shot.crop.unwrap_or(markup::Region {
        x: 0,
        y: 0,
        width,
        height,
    });
    let marked = markup::draw_region(&rgba, width, kept, &shot.marks, false);
    let png = shuttercrab_capture::encode_png(kept.width, kept.height, &marked)?;
    Ok((marked, Arc::new(png)))
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
        pause_hotkeys: Rc::new(move |on_print_screen| {
            // Print Screen is listened for here, as windows never see it go
            // down. The request is sent at once; nothing waits for the
            // answer.
            pause.print_screen.replace(Some(on_print_screen));
            drop(pause.platform.set_hotkeys(print_screen_hotkeys()));
        }),
        apply_hotkeys: Rc::new(move || {
            apply.print_screen.take();
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
            probe.print_screen.take();
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
        windows_takes_print_screen: Rc::new(shuttercrab_platform::windows_takes_print_screen),
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
