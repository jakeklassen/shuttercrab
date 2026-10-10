//! Drives the real settings window in a headless GPUI window, with fake
//! hooks standing in for the rest of Shuttercrab (PRD §26, §33).
//!
//!   cargo test -p shuttercrab --test settings_window

use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, TestAppContext, Window, component::Root, px, size,
    test::TestWindowExt as _,
};
use shuttercrab::{
    app::CAPTURE_BAR_HOTKEY,
    settings::Settings,
    settings_window::{Diagnostics, Hooks, HotkeyField, HotkeyKind, OnPrintScreen, SettingsWindow},
};
use shuttercrab_capture::{MonitorId, MonitorInfo, PhysicalRect};
use shuttercrab_platform::{Hotkey, os::Os};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// What the fake hooks saw.
#[derive(Default)]
struct Seen {
    changed: Cell<u32>,
    paused: Cell<u32>,
    applied: Cell<u32>,
    probed: Cell<u32>,
    /// Hotkey ids the next probe reports as taken.
    taken: RefCell<Vec<u32>>,
    /// What the recording field does with Print Screen.
    print_screen: RefCell<Option<OnPrintScreen>>,
    /// Whether Windows keeps Print Screen, as the window is told.
    print_screen_taken: Cell<bool>,
}

struct Opened {
    handle: AnyWindowHandle,
    settings: Rc<RefCell<Settings>>,
    seen: Rc<Seen>,
}

/// Hooks that record what the window asks of Shuttercrab, on an OS with
/// global hotkeys.
fn hooks() -> (Rc<Hooks>, Rc<RefCell<Settings>>, Rc<Seen>) {
    hooks_on(true)
}

/// As [`hooks`], on an OS with or without `global_hotkeys`, whichever runs
/// the tests.
fn hooks_on(global_hotkeys: bool) -> (Rc<Hooks>, Rc<RefCell<Settings>>, Rc<Seen>) {
    let os: &'static Os = Box::leak(Box::new(Os {
        global_hotkeys,
        ..shuttercrab_platform::os::os().clone()
    }));
    let settings = Rc::new(RefCell::new(Settings::default()));
    let seen = Rc::new(Seen::default());
    let (s1, s2, s3, s4) = (seen.clone(), seen.clone(), seen.clone(), seen.clone());
    let s5 = seen.clone();
    let hooks = Rc::new(Hooks {
        settings: settings.clone(),
        changed: Rc::new(move |_| s1.changed.set(s1.changed.get() + 1)),
        pause_hotkeys: Rc::new(move |on_print_screen| {
            s2.paused.set(s2.paused.get() + 1);
            s2.print_screen.replace(Some(on_print_screen));
        }),
        apply_hotkeys: Rc::new(move || {
            s3.applied.set(s3.applied.get() + 1);
            Box::pin(async { Vec::new() })
        }),
        probe_hotkeys: Rc::new(move || {
            s4.probed.set(s4.probed.get() + 1);
            let taken = std::mem::take(&mut *s4.taken.borrow_mut());
            Box::pin(async move { taken })
        }),
        print_screen_taken: Rc::new(move || s5.print_screen_taken.get()),
        launch_at_startup: Rc::new(|| false),
        set_launch_at_startup: Rc::new(|_| {}),
        diagnostics: Diagnostics {
            version: "0.1.0".into(),
            os,
            monitors: vec![MonitorInfo {
                id: MonitorId::from_raw(1),
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
    (hooks, settings, seen)
}

fn open(cx: &mut TestAppContext) -> Opened {
    open_on(cx, true)
}

fn open_on(cx: &mut TestAppContext, global_hotkeys: bool) -> Opened {
    cx.update(gpui_kit::init);
    let (hooks, settings, seen) = hooks_on(global_hotkeys);
    let handle = cx.open_window(size(px(880.0), px(640.0)), move |window, cx| {
        let view = cx.new(|cx| SettingsWindow::new(hooks, window, cx));
        Root::new(view, window, cx)
    });
    Opened {
        handle: handle.into(),
        settings,
        seen,
    }
}

/// One hotkey field on its own (the Recording page's are not on the page
/// that opens first).
fn open_field(cx: &mut TestAppContext, kind: HotkeyKind) -> Opened {
    cx.update(gpui_kit::init);
    let (hooks, settings, seen) = hooks();
    let handle = cx.open_window(size(px(400.0), px(120.0)), move |window, cx| {
        HotkeyField::new(kind, hooks, window, cx)
    });
    Opened {
        handle: handle.into(),
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
        assert_eq!(
            label(window, "hotkey-screenshot-with-window").as_deref(),
            Some("Ctrl+Alt+Shift+S")
        );
        assert_eq!(
            label(window, "hotkey-record").as_deref(),
            Some("Ctrl+Alt+R")
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
    // Probed with every hotkey, including those taken only while recording.
    assert_eq!(opened.seen.probed.get(), 1);
    assert_eq!(opened.seen.changed.get(), 1);
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "hotkey-capture-bar").as_deref(),
            Some("Ctrl+Alt+X")
        );
    });
}

#[gpui_kit::test]
fn print_screen_alone_can_be_a_hotkey(cx: &mut TestAppContext) {
    let opened = open(cx);
    update(cx, &opened, |window, cx| {
        window.click("hotkey-screenshot", cx)
    });
    // Windows never see Print Screen go down; the app passes it on.
    let on_print_screen = opened.seen.print_screen.borrow().clone().unwrap();
    let print_screen = Hotkey::parse("PrintScreen").unwrap();
    update(cx, &opened, |_, cx| on_print_screen(print_screen, cx));
    assert_eq!(opened.settings.borrow().screenshot_hotkey, "PrintScreen");
    assert_eq!(opened.seen.changed.get(), 1);
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "hotkey-screenshot").as_deref(),
            Some("PrintScreen")
        );
    });
    // Once recorded, another press changes nothing.
    update(cx, &opened, |_, cx| {
        on_print_screen(Hotkey::parse("Shift+PrintScreen").unwrap(), cx)
    });
    assert_eq!(opened.settings.borrow().screenshot_hotkey, "PrintScreen");
}

#[gpui_kit::test]
fn a_field_says_when_windows_keeps_print_screen(cx: &mut TestAppContext) {
    let opened = open(cx);
    let note = "hotkey-screenshot-print-screen";
    update(cx, &opened, |window, _| {
        assert!(window.try_find(note).is_none())
    });
    // Said while a new hotkey is pressed, if Windows keeps Print Screen.
    opened.seen.print_screen_taken.set(true);
    update(cx, &opened, |window, cx| {
        window.click("hotkey-screenshot", cx)
    });
    update(cx, &opened, |window, _| {
        assert!(window.try_find(note).is_some());
        assert!(window.try_find("hotkey-capture-bar-print-screen").is_none());
    });
    // And while the hotkey is Print Screen.
    update(cx, &opened, |window, cx| window.press("escape", cx));
    update(cx, &opened, |window, _| {
        assert!(window.try_find(note).is_none())
    });
    opened.settings.borrow_mut().screenshot_hotkey = "PrintScreen".into();
    update(cx, &opened, |window, _| {
        assert!(window.try_find(note).is_some())
    });
    // Not once Windows lets it go.
    opened.seen.print_screen_taken.set(false);
    update(cx, &opened, |window, _| {
        assert!(window.try_find(note).is_none())
    });
}

#[gpui_kit::test]
fn a_hotkey_another_app_owns_is_refused_and_the_old_one_kept(cx: &mut TestAppContext) {
    let opened = open(cx);
    opened.seen.taken.borrow_mut().push(CAPTURE_BAR_HOTKEY);
    update(cx, &opened, |window, cx| {
        window.click("hotkey-capture-bar", cx);
        window.press("ctrl-alt-x", cx);
    });
    assert_eq!(opened.settings.borrow().capture_bar_hotkey, "Ctrl+Alt+C");
    // Probed for the attempt, then the old one applied again.
    assert_eq!(
        (opened.seen.probed.get(), opened.seen.applied.get()),
        (1, 1)
    );
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
            Some("Ctrl+Alt+S already takes area screenshots.")
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
    assert!(cx.update_window(opened.handle, |_, _, _| ()).is_err());
}

#[gpui_kit::test]
fn recording_chords_are_edited_like_the_others(cx: &mut TestAppContext) {
    let opened = open_field(cx, HotkeyKind::Pause);
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "hotkey-pause").as_deref(), Some("Ctrl+Alt+P"));
    });
    update(cx, &opened, |window, cx| {
        window.click("hotkey-pause", cx);
        window.press("ctrl-alt-f9", cx);
    });
    assert_eq!(opened.settings.borrow().pause_hotkey, "Ctrl+Alt+F9");
    assert_eq!(opened.seen.probed.get(), 1);
    assert_eq!(opened.seen.changed.get(), 1);
}

#[gpui_kit::test]
fn every_hotkey_is_checked_against_every_other(cx: &mut TestAppContext) {
    let opened = open_field(cx, HotkeyKind::Discard);
    // Recording's own hotkey.
    update(cx, &opened, |window, cx| {
        window.click("hotkey-discard", cx);
        window.press("ctrl-alt-r", cx);
    });
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "hotkey-discard-message").as_deref(),
            Some("Ctrl+Alt+R already starts and stops recording.")
        );
    });
    // Another chord taken only while recording.
    update(cx, &opened, |window, cx| window.press("ctrl-alt-z", cx));
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "hotkey-discard-message").as_deref(),
            Some("Ctrl+Alt+Z already undoes a discard.")
        );
    });
    assert_eq!(opened.settings.borrow().discard_hotkey, "Ctrl+Alt+D");
    assert_eq!(opened.seen.changed.get(), 0);
}

#[gpui_kit::test]
fn a_recording_chord_another_app_owns_is_refused(cx: &mut TestAppContext) {
    let opened = open_field(cx, HotkeyKind::Undo);
    opened
        .seen
        .taken
        .borrow_mut()
        .push(shuttercrab::app::UNDO_HOTKEY);
    update(cx, &opened, |window, cx| {
        window.click("hotkey-undo", cx);
        window.press("ctrl-alt-u", cx);
    });
    assert_eq!(opened.settings.borrow().undo_hotkey, "Ctrl+Alt+Z");
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "hotkey-undo-message").as_deref(),
            Some("Ctrl+Alt+U is used by another application.")
        );
    });
}

#[gpui_kit::test]
fn backing_out_of_a_refused_hotkey_clears_the_message(cx: &mut TestAppContext) {
    let opened = open_field(cx, HotkeyKind::Undo);
    update(cx, &opened, |window, cx| {
        window.click("hotkey-undo", cx);
        window.press("ctrl-alt-r", cx);
    });
    update(cx, &opened, |window, _| {
        assert!(label(window, "hotkey-undo-message").is_some());
    });
    update(cx, &opened, |window, cx| window.press("escape", cx));
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "hotkey-undo-message"), None);
        assert_eq!(label(window, "hotkey-undo").as_deref(), Some("Ctrl+Alt+Z"));
    });
}

#[gpui_kit::test]
fn without_global_hotkeys_it_shows_the_commands_for_desktop_shortcuts(cx: &mut TestAppContext) {
    let opened = open_on(cx, false);
    update(cx, &opened, |window, cx| {
        assert!(window.try_find("hotkey-capture-bar").is_none());
        window.click("copy-screenshot", cx);
    });
    let copied = cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(
        copied,
        Some(shuttercrab::app::shortcut_command(
            shuttercrab_platform::Request::Screenshot
        ))
    );
    assert!(copied.unwrap().ends_with(" --screenshot"));
}
