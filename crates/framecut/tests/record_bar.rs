//! Drives the recording controls in a headless GPUI window with native
//! pointer and keyboard events (PRD §16).
//!
//!   cargo test -p framecut --test record_bar
#![cfg(windows)]

use framecut::{
    record_bar::{
        BarMode, Destructive, RECORD_BAR_HEIGHT, RECORD_BAR_WIDTH, RecordBar, RecordBarEvent,
        RecordKeys,
    },
    recording::Clock,
};
use gpui_kit::{
    App, AppContext as _, TestAppContext, VisualTestContext, Window, WindowHandle, px, size,
    test::TestWindowExt as _,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

struct Opened {
    handle: WindowHandle<RecordBar>,
    clock: Rc<Cell<Clock>>,
    events: Rc<RefCell<Vec<RecordBarEvent>>>,
}

/// Controls for a recording `running` long.
fn open(cx: &mut TestAppContext, running: Duration) -> Opened {
    cx.update(gpui_kit::init);
    let clock = Rc::new(Cell::new(Clock::new(Instant::now() - running)));
    let events = Rc::new(RefCell::new(Vec::new()));
    let (sink, shared) = (events.clone(), clock.clone());
    let handle = cx.open_window(
        size(px(RECORD_BAR_WIDTH), px(RECORD_BAR_HEIGHT)),
        move |window, cx| {
            cx.subscribe_self(move |_, event: &RecordBarEvent, _| sink.borrow_mut().push(*event))
                .detach();
            let keys = RecordKeys {
                pause: "Ctrl+Alt+P".into(),
                stop: "Ctrl+Alt+R".into(),
                restart: "Ctrl+Alt+N".into(),
                discard: "Ctrl+Alt+D".into(),
                undo: "Ctrl+Alt+Z".into(),
            };
            RecordBar::new(shared, keys, window, cx)
        },
    );
    Opened {
        handle,
        clock,
        events,
    }
}

fn update(cx: &mut TestAppContext, opened: &Opened, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(opened.handle.into(), |_, window, cx| {
        window.render_frame(cx);
        f(window, cx);
    })
    .unwrap();
}

fn label(window: &Window, id: &'static str) -> Option<String> {
    window
        .try_find(id)
        .filter(|e| e.visible())
        .and_then(|e| e.label().map(|l| l.to_string()))
}

fn events(opened: &Opened) -> Vec<RecordBarEvent> {
    opened.events.borrow().clone()
}

/// Show `mode` on the bar.
fn set_mode(cx: &mut TestAppContext, opened: &Opened, mode: BarMode) {
    cx.update(|cx| {
        opened
            .handle
            .update(cx, |bar, _, cx| bar.set_mode(mode, cx))
            .unwrap()
    });
}

fn mode(cx: &mut TestAppContext, opened: &Opened) -> BarMode {
    cx.update(|cx| opened.handle.read(cx).unwrap().mode())
}

/// Give the bar the keyboard, as a click does.
fn activate(cx: &mut TestAppContext, opened: &Opened) {
    update(cx, opened, |window, _| window.activate_window());
    cx.run_until_parked();
}

#[gpui_kit::test]
fn shows_the_time_and_all_four_actions(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::from_secs(84));
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "record-time").unwrap(), "1:24");
        for id in [
            "record-pause",
            "record-stop",
            "record-restart",
            "record-discard",
        ] {
            assert!(window.try_find(id).is_some(), "{id}");
        }
    });
}

#[gpui_kit::test]
fn a_paused_recording_offers_resume(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::from_secs(5));
    let mut clock = opened.clock.get();
    clock.pause(Instant::now());
    opened.clock.set(clock);
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "record-time").unwrap(), "0:05");
        assert_eq!(label(window, "record-pause").unwrap(), "Resume");
    });
}

#[gpui_kit::test]
fn hints_show_the_chords_until_the_bar_has_the_keyboard(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::ZERO);
    // As it appears: the app being recorded keeps the keyboard.
    VisualTestContext::from_window(opened.handle.into(), cx).deactivate_window();
    update(cx, &opened, |window, _| {
        for (id, chord) in [
            ("record-pause-key", "Ctrl+Alt+P"),
            ("record-stop-key", "Ctrl+Alt+R"),
            ("record-restart-key", "Ctrl+Alt+N"),
            ("record-discard-key", "Ctrl+Alt+D"),
        ] {
            assert_eq!(label(window, id).unwrap(), chord, "{id}");
        }
    });
    // Clicked: the letters work, and say so.
    activate(cx, &opened);
    update(cx, &opened, |window, _| {
        for (id, letter) in [
            ("record-pause-key", "P"),
            ("record-stop-key", "S"),
            ("record-restart-key", "N"),
            ("record-discard-key", "D"),
        ] {
            assert_eq!(label(window, id).unwrap(), letter, "{id}");
        }
    });
}

#[gpui_kit::test]
fn letters_ask_for_each_action(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::ZERO);
    activate(cx, &opened);
    update(cx, &opened, |window, cx| {
        for key in ["p", "space", "s", "n", "d", "z", "enter", "escape"] {
            window.press(key, cx);
        }
    });
    // Z, Enter and Escape mean nothing until there is something to undo
    // or a question.
    assert_eq!(
        events(&opened),
        [
            RecordBarEvent::TogglePause,
            RecordBarEvent::TogglePause,
            RecordBarEvent::Stop,
            RecordBarEvent::Restart,
            RecordBarEvent::Discard,
        ]
    );
}

#[gpui_kit::test]
fn clicking_the_buttons(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::ZERO);
    for id in [
        "record-pause",
        "record-stop",
        "record-restart",
        "record-discard",
    ] {
        update(cx, &opened, |window, cx| window.click(id, cx));
    }
    assert_eq!(
        events(&opened),
        [
            RecordBarEvent::TogglePause,
            RecordBarEvent::Stop,
            RecordBarEvent::Restart,
            RecordBarEvent::Discard,
        ]
    );
}

#[gpui_kit::test]
fn asking_before_a_discard(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::from_secs(134));
    set_mode(
        cx,
        &opened,
        BarMode::Confirm(Destructive::Discard, Duration::from_secs(134)),
    );
    activate(cx, &opened);
    update(cx, &opened, |window, cx| {
        assert_eq!(
            label(window, "record-time").unwrap(),
            "Discard this 2:14 recording?"
        );
        assert_eq!(label(window, "record-keep-key").unwrap(), "Esc");
        assert_eq!(label(window, "record-confirm-key").unwrap(), "Enter");
        assert!(window.try_find("record-pause").is_none());
        // The other actions' letters do nothing while it asks.
        window.press("p", cx);
        window.press("n", cx);
        window.press("escape", cx);
        window.press("enter", cx);
        window.press("d", cx);
    });
    assert_eq!(
        events(&opened),
        [
            RecordBarEvent::Cancel,
            RecordBarEvent::Confirm,
            RecordBarEvent::Confirm
        ]
    );
    // Without the keyboard, the hints are the chords that answer.
    VisualTestContext::from_window(opened.handle.into(), cx).deactivate_window();
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "record-confirm-key").unwrap(), "Ctrl+Alt+D");
        assert_eq!(label(window, "record-keep-key").unwrap(), "Ctrl+Alt+P");
    });
}

#[gpui_kit::test]
fn asking_before_a_restart(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::from_secs(61));
    set_mode(
        cx,
        &opened,
        BarMode::Confirm(Destructive::Restart, Duration::from_secs(61)),
    );
    activate(cx, &opened);
    update(cx, &opened, |window, cx| {
        assert_eq!(
            label(window, "record-time").unwrap(),
            "Restart? The 1:01 take is thrown away."
        );
        window.press("d", cx);
        window.press("n", cx);
    });
    assert_eq!(events(&opened), [RecordBarEvent::Confirm]);
}

#[gpui_kit::test]
fn a_discard_counts_down_and_offers_undo(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::from_secs(30));
    let until = Instant::now() + Duration::from_millis(9_500);
    set_mode(cx, &opened, BarMode::Discarded { until });
    activate(cx, &opened);
    update(cx, &opened, |window, cx| {
        assert_eq!(
            label(window, "record-time").unwrap(),
            "Discarded · deleted in 10 s"
        );
        assert_eq!(label(window, "record-undo-key").unwrap(), "Z");
        assert!(window.try_find("record-discard").is_none());
        window.press("d", cx);
        window.press("z", cx);
    });
    assert_eq!(events(&opened), [RecordBarEvent::Undo]);
    assert_eq!(mode(cx, &opened), BarMode::Discarded { until });
}

#[gpui_kit::test]
fn after_a_restart_the_previous_take_can_be_kept(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::ZERO);
    cx.update(|cx| {
        opened
            .handle
            .update(cx, |bar, _, cx| {
                bar.offer_previous(Some(Instant::now() + Duration::from_secs(10)), cx)
            })
            .unwrap()
    });
    activate(cx, &opened);
    update(cx, &opened, |window, cx| {
        assert_eq!(
            label(window, "record-keep-previous").unwrap(),
            "Keep previous take"
        );
        // It takes Restart's place.
        assert!(window.try_find("record-restart").is_none());
        window.press("n", cx);
        window.press("z", cx);
    });
    assert_eq!(events(&opened), [RecordBarEvent::Undo]);
    // Once withdrawn, Restart is back.
    cx.update(|cx| {
        opened
            .handle
            .update(cx, |bar, _, cx| bar.offer_previous(None, cx))
            .unwrap()
    });
    update(cx, &opened, |window, _| {
        assert!(window.try_find("record-restart").is_some());
        assert!(window.try_find("record-keep-previous").is_none());
    });
}
