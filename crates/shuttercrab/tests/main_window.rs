//! Drives the real main window in a headless GPUI window, with fake hooks
//! standing in for the rest of Shuttercrab.
//!
//!   cargo test -p shuttercrab --test main_window
#![cfg(windows)]

use gpui_kit::{
    App, AppContext as _, Entity, TestAppContext, Window, component::Root, test::TestWindowExt as _,
};
use shuttercrab::{
    capture_bar::{CaptureMode, CaptureTarget},
    main_window::{HOME_SIZE, MainHooks, MainWindow, Page},
    settings::Settings,
    settings_window::{Diagnostics, Hooks},
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// What the fake hooks saw.
#[derive(Default)]
struct Seen {
    captures: RefCell<Vec<(CaptureMode, CaptureTarget)>>,
    folders: Cell<u32>,
    quits: Cell<u32>,
    restarts: Cell<u32>,
    /// The version the fake updater has ready, if any.
    update: RefCell<Option<String>>,
}

struct Opened {
    handle: gpui_kit::AnyWindowHandle,
    view: Entity<MainWindow>,
    settings: Rc<RefCell<Settings>>,
    seen: Rc<Seen>,
}

fn open(cx: &mut TestAppContext) -> Opened {
    cx.update(gpui_kit::init);
    let settings = Rc::new(RefCell::new(Settings::default()));
    let seen = Rc::new(Seen::default());
    let (s1, s2, s3) = (seen.clone(), seen.clone(), seen.clone());
    let (s4, s5) = (seen.clone(), seen.clone());
    let hooks = Rc::new(MainHooks {
        settings: Rc::new(Hooks {
            settings: settings.clone(),
            changed: Rc::new(|_| {}),
            pause_hotkeys: Rc::new(|| {}),
            apply_hotkeys: Rc::new(|| Box::pin(async { Vec::new() })),
            probe_hotkeys: Rc::new(|| Box::pin(async { Vec::new() })),
            launch_at_startup: Rc::new(|| false),
            set_launch_at_startup: Rc::new(|_| {}),
            diagnostics: Diagnostics::default(),
        }),
        capture: Rc::new(move |mode, target, _, _| s1.captures.borrow_mut().push((mode, target))),
        open_folder: Rc::new(move |_| s2.folders.set(s2.folders.get() + 1)),
        quit: Rc::new(move |_| s3.quits.set(s3.quits.get() + 1)),
        update_ready: Rc::new(move || s4.update.borrow().clone()),
        restart_to_update: Rc::new(move |_| s5.restarts.set(s5.restarts.get() + 1)),
    });
    let out = Rc::new(RefCell::new(None));
    let slot = out.clone();
    let handle = cx.open_window(HOME_SIZE, move |window, cx| {
        let view = cx.new(|cx| MainWindow::new(hooks, window, cx));
        slot.replace(Some(view.clone()));
        Root::new(view, window, cx)
    });
    let view = out.borrow_mut().take().unwrap();
    Opened {
        handle: handle.into(),
        view,
        settings,
        seen,
    }
}

fn update(cx: &mut TestAppContext, opened: &Opened, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(opened.handle, |_, window, cx| {
        window.render_frame(cx);
        f(window, cx);
    })
    .unwrap();
}

fn press(cx: &mut TestAppContext, opened: &Opened, keys: &[&str]) {
    for key in keys {
        update(cx, opened, |window, cx| window.press(key, cx));
    }
}

fn choice(opened: &Opened) -> (CaptureMode, CaptureTarget, u32, u32) {
    let s = opened.settings.borrow();
    (
        s.last_mode,
        s.last_target,
        s.screenshot_delay,
        s.recording_countdown,
    )
}

#[gpui_kit::test]
fn letters_pick_the_mode_and_target_and_n_starts(cx: &mut TestAppContext) {
    let opened = open(cx);
    press(cx, &opened, &["f"]);
    assert_eq!(choice(&opened).1, CaptureTarget::Freeform);
    press(cx, &opened, &["n"]);
    press(cx, &opened, &["r", "w", "enter"]);
    // Record offers no Window or Freeform: it records an area.
    assert_eq!(
        *opened.seen.captures.borrow(),
        [
            (CaptureMode::Screenshot, CaptureTarget::Freeform),
            (CaptureMode::Record, CaptureTarget::Area)
        ]
    );
    press(cx, &opened, &["d", "s"]);
    assert_eq!(choice(&opened).0, CaptureMode::Screenshot);
    assert_eq!(choice(&opened).1, CaptureTarget::Display);
}

#[gpui_kit::test]
fn clicking_works_like_the_keys(cx: &mut TestAppContext) {
    let opened = open(cx);
    update(cx, &opened, |window, cx| {
        window.click("mode-record", cx);
        window.click("target-display", cx);
        window.click("new", cx);
    });
    assert_eq!(
        *opened.seen.captures.borrow(),
        [(CaptureMode::Record, CaptureTarget::Display)]
    );
}

#[gpui_kit::test]
fn t_steps_through_the_delays_for_the_mode(cx: &mut TestAppContext) {
    let opened = open(cx);
    press(cx, &opened, &["t", "t"]);
    assert_eq!(choice(&opened).2, 5);
    press(cx, &opened, &["t", "t"]);
    assert_eq!(choice(&opened).2, 0);
    // Recordings step through the countdowns, and keep their own.
    press(cx, &opened, &["r", "t"]);
    assert_eq!((choice(&opened).2, choice(&opened).3), (0, 3));
}

#[gpui_kit::test]
fn the_delay_menu_chooses_by_click_or_keys(cx: &mut TestAppContext) {
    let opened = open(cx);
    update(cx, &opened, |window, cx| window.click("delay", cx));
    update(cx, &opened, |window, _| {
        assert!(window.try_find("delay-menu").is_some())
    });
    update(cx, &opened, |window, cx| window.click("delay-3", cx));
    assert_eq!(choice(&opened).2, 10);
    update(cx, &opened, |window, _| {
        assert!(window.try_find("delay-menu").is_none())
    });
    update(cx, &opened, |window, cx| window.click("delay", cx));
    press(cx, &opened, &["down", "enter"]);
    assert_eq!(choice(&opened).2, 0);
    // Escape closes the menu and changes nothing.
    update(cx, &opened, |window, cx| window.click("delay", cx));
    press(cx, &opened, &["escape"]);
    update(cx, &opened, |window, _| {
        assert!(window.try_find("delay-menu").is_none())
    });
    assert_eq!(choice(&opened).2, 0);
}

#[gpui_kit::test]
fn settings_open_in_the_window_and_escape_comes_back(cx: &mut TestAppContext) {
    let opened = open(cx);
    let page = |cx: &mut TestAppContext| cx.update(|cx| opened.view.read(cx).page());
    press(cx, &opened, &[","]);
    assert_eq!(page(cx), Page::Settings);
    update(cx, &opened, |window, _| {
        assert!(window.try_find("settings-window").is_some());
        assert!(window.try_find("toolbar").is_none());
    });
    press(cx, &opened, &["escape"]);
    assert_eq!(page(cx), Page::Home);
    // And through the ⋯ menu and the back button.
    update(cx, &opened, |window, cx| window.click("more", cx));
    update(cx, &opened, |window, cx| window.click("more-0", cx));
    assert_eq!(page(cx), Page::Settings);
    update(cx, &opened, |window, cx| window.click("back", cx));
    assert_eq!(page(cx), Page::Home);
}

#[gpui_kit::test]
fn the_folder_and_quit_have_keys(cx: &mut TestAppContext) {
    let opened = open(cx);
    press(cx, &opened, &["o", "q"]);
    assert_eq!(opened.seen.folders.get(), 1);
    // Q alone does nothing; Ctrl+Q quits.
    assert_eq!(opened.seen.quits.get(), 0);
    press(cx, &opened, &["ctrl-q"]);
    assert_eq!(opened.seen.quits.get(), 1);
}

#[gpui_kit::test]
fn the_version_shows_until_an_update_is_ready_then_u_restarts(cx: &mut TestAppContext) {
    let opened = open(cx);
    let label = |window: &Window, id: &'static str| {
        window
            .try_find(id)
            .and_then(|e| e.label().map(|l| l.to_string()))
    };
    update(cx, &opened, |window, _| {
        let expected = format!("Shuttercrab {}", env!("CARGO_PKG_VERSION"));
        assert_eq!(label(window, "version"), Some(expected));
        assert!(window.try_find("update").is_none());
    });
    // U does nothing without an update.
    press(cx, &opened, &["u"]);
    assert_eq!(opened.seen.restarts.get(), 0);

    opened.seen.update.replace(Some("9.9.9".into()));
    update(cx, &opened, |window, _| {
        assert!(window.try_find("version").is_none());
        assert_eq!(
            label(window, "update").as_deref(),
            Some("Restart to update to 9.9.9")
        );
    });
    press(cx, &opened, &["u"]);
    update(cx, &opened, |window, cx| window.click("update", cx));
    assert_eq!(opened.seen.restarts.get(), 2);
}
