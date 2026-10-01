//! Drives the countdown before recording in a headless GPUI window (PRD
//! §7.7).
//!
//!   cargo test -p framecut --test countdown
#![cfg(windows)]

use framecut::countdown::{COUNTDOWN_HEIGHT, COUNTDOWN_WIDTH, Countdown, CountdownEvent};
use gpui_kit::{AppContext as _, TestAppContext, WindowHandle, px, size, test::TestWindowExt as _};
use std::{cell::RefCell, rc::Rc, time::Duration};

struct Opened {
    handle: WindowHandle<Countdown>,
    events: Rc<RefCell<Vec<CountdownEvent>>>,
}

fn open(cx: &mut TestAppContext, seconds: u32) -> Opened {
    cx.update(gpui_kit::init);
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    let handle = cx.open_window(
        size(px(COUNTDOWN_WIDTH), px(COUNTDOWN_HEIGHT)),
        move |window, cx| {
            cx.subscribe_self(move |_, event: &CountdownEvent, _| sink.borrow_mut().push(*event))
                .detach();
            Countdown::new(seconds, window, cx)
        },
    );
    Opened { handle, events }
}

fn left(cx: &mut TestAppContext, opened: &Opened) -> u32 {
    cx.update(|cx| opened.handle.read(cx).unwrap().left())
}

fn press(cx: &mut TestAppContext, opened: &Opened, key: &str) {
    cx.update_window(opened.handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press(key, cx);
    })
    .unwrap();
}

#[gpui_kit::test]
fn counts_down_a_second_at_a_time_then_goes(cx: &mut TestAppContext) {
    let opened = open(cx, 3);
    assert_eq!(left(cx, &opened), 3);
    cx.executor().advance_clock(Duration::from_secs(1));
    assert_eq!(left(cx, &opened), 2);
    cx.executor().advance_clock(Duration::from_secs(1));
    assert_eq!(left(cx, &opened), 1);
    assert!(opened.events.borrow().is_empty());
    cx.executor().advance_clock(Duration::from_secs(1));
    assert_eq!(*opened.events.borrow(), [CountdownEvent::Go]);
    // Nothing more after it ends.
    cx.executor().advance_clock(Duration::from_secs(3));
    assert_eq!(opened.events.borrow().len(), 1);
}

#[gpui_kit::test]
fn enter_starts_at_once_and_escape_cancels(cx: &mut TestAppContext) {
    let opened = open(cx, 5);
    press(cx, &opened, "enter");
    press(cx, &opened, "escape");
    assert_eq!(*opened.events.borrow(), [CountdownEvent::Go]);

    let opened = open(cx, 5);
    press(cx, &opened, "escape");
    cx.executor().advance_clock(Duration::from_secs(6));
    assert_eq!(*opened.events.borrow(), [CountdownEvent::Cancel]);
}
