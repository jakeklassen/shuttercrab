//! Drives the real settings window in a headless GPUI window, with fake
//! hooks standing in for the rest of Framecut (PRD §26, §33).
//!
//!   cargo test -p framecut --test settings_window
#![cfg(windows)]

use framecut::{
    settings::Settings,
    settings_window::{Diagnostics, Hooks, SettingsWindow},
};
use framecut_capture::{MonitorId, MonitorInfo, PhysicalRect};
use gpui_kit::{
    App, AppContext as _, TestAppContext, Window, WindowHandle, component::Root, px, size,
    test::TestWindowExt as _,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

const CAPTURE_BAR: u32 = 3;
const SCREENSHOT: u32 = 1;

/// What the fake hooks saw.
#[derive(Default)]
struct Seen {
    changed: Cell<u32>,
    paused: Cell<u32>,
    applied: Cell<u32>,
    /// Hotkey ids the next apply reports as taken.
    taken: RefCell<Vec<u32>>,
}

struct Opened {
    handle: WindowHandle<Root>,
    settings: Rc<RefCell<Settings>>,
    seen: Rc<Seen>,
}

fn open(cx: &mut TestAppContext) -> Opened {
    cx.update(gpui_kit::init);
    let settings = Rc::new(RefCell::new(Settings::default()));
    let seen = Rc::new(Seen::default());
    let (s1, s2, s3) = (seen.clone(), seen.clone(), seen.clone());
    let hooks = Rc::new(Hooks {
        settings: settings.clone(),
        changed: Rc::new(move |_| s1.changed.set(s1.changed.get() + 1)),
        pause_hotkeys: Rc::new(move || s2.paused.set(s2.paused.get() + 1)),
        apply_hotkeys: Rc::new(move || {
            s3.applied.set(s3.applied.get() + 1);
            let taken = std::mem::take(&mut *s3.taken.borrow_mut());
            Box::pin(async move { taken })
        }),
        launch_at_startup: Rc::new(|| false),
        set_launch_at_startup: Rc::new(|_| {}),
        diagnostics: Diagnostics {
            version: "0.1.0".into(),
            windows_build: 26200,
            monitors: vec![MonitorInfo {
                id: MonitorId(1),
                device_name: r"\\.\DISPLAY1".into(),
                name: "Test Monitor".into(),
                bounds: PhysicalRect::new(0, 0, 3840, 2160),
                scale_factor: 1.5,
                advanced_color_enabled: true,
                hdr_enabled: true,
                sdr_white_level_nits: Some(240.0),
                adapter: "Test GPU".into(),
            }],
            log_dir: None,
            settings_path: None,
        },
    });
    let handle = cx.open_window(size(px(880.0), px(640.0)), move |window, cx| {
        let view = cx.new(|cx| SettingsWindow::new(hooks, (CAPTURE_BAR, SCREENSHOT), window, cx));
        Root::new(view, window, cx)
    });
    Opened {
        handle,
        settings,
        seen,
    }
}

fn update(cx: &mut TestAppContext, opened: &Opened, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(opened.handle.into(), |_, window, cx| {
        window.render_frame(cx);
        f(window, cx);
    })
    .unwrap();
    cx.run_until_parked();
}

fn label(window: &Window, id: &'static str) -> Option<String> {
    window
        .try_find(id)
        .and_then(|e| e.label().map(|l| l.to_string()))
}

#[gpui_kit::test]
fn shows_the_hotkeys_from_the_settings(cx: &mut TestAppContext) {
    let opened = open(cx);
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "hotkey-capture-bar").as_deref(),
            Some("Ctrl+Alt+C")
        );
        assert_eq!(
            label(window, "hotkey-screenshot").as_deref(),
            Some("Ctrl+Alt+S")
        );
    });
}

#[gpui_kit::test]
fn recording_a_hotkey_pauses_hotkeys_then_applies_the_new_one(cx: &mut TestAppContext) {
    let opened = open(cx);
    update(cx, &opened, |window, cx| {
        window.click("hotkey-capture-bar", cx)
    });
    assert_eq!(opened.seen.paused.get(), 1);
    update(cx, &opened, |window, _| {
        assert!(
            label(window, "hotkey-capture-bar")
                .unwrap()
                .starts_with("Press the new shortcut")
        );
    });
    update(cx, &opened, |window, cx| window.press("ctrl-alt-x", cx));
    assert_eq!(opened.settings.borrow().capture_bar_hotkey, "Ctrl+Alt+X");
    assert_eq!(opened.seen.applied.get(), 1);
    assert_eq!(opened.seen.changed.get(), 1);
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "hotkey-capture-bar").as_deref(),
            Some("Ctrl+Alt+X")
        );
    });
}

#[gpui_kit::test]
fn a_hotkey_another_app_owns_is_refused_and_the_old_one_kept(cx: &mut TestAppContext) {
    let opened = open(cx);
    opened.seen.taken.borrow_mut().push(CAPTURE_BAR);
    update(cx, &opened, |window, cx| {
        window.click("hotkey-capture-bar", cx);
        window.press("ctrl-alt-x", cx);
    });
    assert_eq!(opened.settings.borrow().capture_bar_hotkey, "Ctrl+Alt+C");
    // Applied once for the attempt, once to restore the old one.
    assert_eq!(opened.seen.applied.get(), 2);
    assert_eq!(opened.seen.changed.get(), 0);
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "hotkey-capture-bar-message").as_deref(),
            Some("Ctrl+Alt+X is used by another application.")
        );
    });
}

#[gpui_kit::test]
fn the_other_hotkey_and_bare_keys_are_refused(cx: &mut TestAppContext) {
    let opened = open(cx);
    update(cx, &opened, |window, cx| {
        window.click("hotkey-capture-bar", cx);
        window.press("ctrl-alt-s", cx);
    });
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "hotkey-capture-bar-message").as_deref(),
            Some("Ctrl+Alt+S already opens area screenshots.")
        );
    });
    update(cx, &opened, |window, cx| window.press("shift-x", cx));
    update(cx, &opened, |window, _| {
        assert!(
            label(window, "hotkey-capture-bar-message")
                .unwrap()
                .starts_with("Include Ctrl, Alt or Win")
        );
    });
    assert_eq!(opened.settings.borrow().capture_bar_hotkey, "Ctrl+Alt+C");
    assert_eq!(opened.seen.changed.get(), 0);
}

#[gpui_kit::test]
fn escape_cancels_recording_then_closes_the_window(cx: &mut TestAppContext) {
    let opened = open(cx);
    update(cx, &opened, |window, cx| {
        window.click("hotkey-screenshot", cx);
        window.press("escape", cx);
    });
    // The recording ended and the hotkeys were registered again; the window
    // is still open.
    assert_eq!(opened.seen.applied.get(), 1);
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "hotkey-screenshot").as_deref(),
            Some("Ctrl+Alt+S")
        );
    });
    update(cx, &opened, |window, cx| window.press("escape", cx));
    // Hidden at once, removed a moment later.
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(200));
    assert!(
        cx.update_window(opened.handle.into(), |_, _, _| ())
            .is_err()
    );
}
