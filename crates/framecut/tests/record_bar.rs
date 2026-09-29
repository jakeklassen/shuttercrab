//! Drives the recording controls in a headless GPUI window with native
//! pointer and keyboard events (PRD §16).
//!
//!   cargo test -p framecut --test record_bar
#![cfg(windows)]

use framecut::{
    record_bar::{RECORD_BAR_HEIGHT, RECORD_BAR_WIDTH, RecordBar, RecordBarEvent},
    recording::Clock,
};
use gpui_kit::{
    App, AppContext as _, TestAppContext, Window, WindowHandle, px, size, test::TestWindowExt as _,
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
            RecordBar::new(shared, window, cx)
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

#[gpui_kit::test]
fn shows_the_elapsed_time_and_the_main_buttons(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::from_secs(84));
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "record-time").unwrap(), "Recording 1:24");
        for id in ["record-pause", "record-stop", "record-more"] {
            assert!(window.try_find(id).is_some(), "{id}");
        }
        assert!(window.try_find("record-discard").is_none());
    });
}

#[gpui_kit::test]
fn a_paused_recording_says_so_and_offers_resume(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::from_secs(5));
    let mut clock = opened.clock.get();
    clock.pause(Instant::now());
    opened.clock.set(clock);
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "record-time").unwrap(), "Paused at 0:05");
        assert_eq!(label(window, "record-pause").unwrap(), "Resume");
    });
}

#[gpui_kit::test]
fn keys_pause_and_stop(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::ZERO);
    update(cx, &opened, |window, cx| {
        window.press("p", cx);
        window.press("space", cx);
        window.press("s", cx);
    });
    assert_eq!(
        events(&opened),
        [
            RecordBarEvent::TogglePause,
            RecordBarEvent::TogglePause,
            RecordBarEvent::Stop
        ]
    );
}

#[gpui_kit::test]
fn restart_and_discard_take_two_steps(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::ZERO);
    // Not from the main row.
    update(cx, &opened, |window, cx| {
        window.press("r", cx);
        window.press("d", cx);
    });
    assert!(events(&opened).is_empty());
    // M opens the menu, Escape closes it, and it does nothing else.
    update(cx, &opened, |window, cx| window.press("m", cx));
    update(cx, &opened, |window, _| {
        assert!(window.try_find("record-restart").is_some());
        assert!(window.try_find("record-pause").is_none());
    });
    update(cx, &opened, |window, cx| window.press("escape", cx));
    assert!(!cx.update(|cx| opened.handle.read(cx).unwrap().menu_open()));
    // M then R restarts; M then D discards; the menu closes after each.
    update(cx, &opened, |window, cx| {
        window.press("m", cx);
        window.press("r", cx);
    });
    update(cx, &opened, |window, cx| {
        window.press("m", cx);
        window.press("d", cx);
    });
    assert_eq!(
        events(&opened),
        [RecordBarEvent::Restart, RecordBarEvent::Discard]
    );
    assert!(!cx.update(|cx| opened.handle.read(cx).unwrap().menu_open()));
}

#[gpui_kit::test]
fn clicking_the_buttons(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::ZERO);
    update(cx, &opened, |window, cx| window.click("record-pause", cx));
    update(cx, &opened, |window, cx| window.click("record-more", cx));
    update(cx, &opened, |window, cx| window.click("record-discard", cx));
    update(cx, &opened, |window, cx| window.click("record-stop", cx));
    assert_eq!(
        events(&opened),
        [
            RecordBarEvent::TogglePause,
            RecordBarEvent::Discard,
            RecordBarEvent::Stop
        ]
    );
}
