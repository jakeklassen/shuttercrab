//! Drives the real post-capture thumbnail in a headless GPUI window with
//! native pointer events (PRD §7.6, §33).
//!
//!   cargo test -p framecut --test thumbnail
#![cfg(windows)]

use framecut::thumbnail::{Thumbnail, ThumbnailEvent, render_image, scale_down};
use gpui_kit::{
    App, AppContext as _, InputEvent as _, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, TestAppContext, Window, WindowHandle, point, px, size,
    test::TestWindowExt as _,
};
use std::{cell::RefCell, rc::Rc, time::Duration};

struct Opened {
    handle: WindowHandle<Thumbnail>,
    events: Rc<RefCell<Vec<ThumbnailEvent>>>,
}

fn open(cx: &mut TestAppContext, seconds: u32) -> Opened {
    cx.update(gpui_kit::init);
    let image = render_image(&scale_down(vec![200; 64 * 36 * 4], 64, 36, 64, 36));
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    let handle = cx.open_window(size(px(252.0), px(147.0)), move |_, cx| {
        cx.subscribe_self(move |_, event: &ThumbnailEvent, _| sink.borrow_mut().push(*event))
            .detach();
        Thumbnail::new(image, seconds, cx)
    });
    let opened = Opened { handle, events };
    // The headless pointer starts at (0, 0), over the card; move it away,
    // as it would be after a capture.
    update(cx, &opened, |window, cx| {
        pointer(window, point(px(-50.0), px(-50.0)), false, cx)
    });
    opened
}

fn update(cx: &mut TestAppContext, opened: &Opened, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(opened.handle.into(), |_, window, cx| {
        window.render_frame(cx);
        f(window, cx);
    })
    .unwrap();
}

fn pointer(window: &mut Window, position: Point<Pixels>, pressed: bool, cx: &mut App) {
    window.dispatch_event(
        MouseMoveEvent {
            position,
            pressed_button: pressed.then_some(MouseButton::Left),
            modifiers: Default::default(),
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
}

fn button(window: &mut Window, position: Point<Pixels>, down: bool, cx: &mut App) {
    let event = if down {
        MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        }
        .to_platform_input()
    } else {
        MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Default::default(),
            click_count: 1,
        }
        .to_platform_input()
    };
    window.dispatch_event(event, cx);
    window.render_frame(cx);
}

#[gpui_kit::test]
fn a_click_opens_the_screenshot(cx: &mut TestAppContext) {
    let opened = open(cx, 6);
    update(cx, &opened, |window, cx| window.click("thumbnail", cx));
    assert_eq!(*opened.events.borrow(), [ThumbnailEvent::Open]);
}

#[gpui_kit::test]
fn pressing_and_moving_starts_a_drag_not_a_click(cx: &mut TestAppContext) {
    let opened = open(cx, 6);
    let start = point(px(100.0), px(70.0));
    update(cx, &opened, |window, cx| {
        pointer(window, start, false, cx);
        button(window, start, true, cx);
        // Small jitter is still a click…
        pointer(window, point(px(102.0), px(71.0)), true, cx);
    });
    assert!(opened.events.borrow().is_empty());
    update(cx, &opened, |window, cx| {
        // …a real move is a drag, reported once.
        pointer(window, point(px(120.0), px(80.0)), true, cx);
        pointer(window, point(px(140.0), px(90.0)), true, cx);
        button(window, point(px(140.0), px(90.0)), false, cx);
    });
    assert_eq!(*opened.events.borrow(), [ThumbnailEvent::Drag]);
}

#[gpui_kit::test]
fn the_close_button_appears_on_hover_and_closes(cx: &mut TestAppContext) {
    let opened = open(cx, 6);
    update(cx, &opened, |window, _| {
        assert!(window.try_find("thumbnail-close").is_none());
    });
    update(cx, &opened, |window, cx| window.hover("thumbnail", cx));
    update(cx, &opened, |window, cx| {
        window.click("thumbnail-close", cx)
    });
    assert_eq!(*opened.events.borrow(), [ThumbnailEvent::Close]);
}

#[gpui_kit::test]
fn it_closes_itself_after_the_countdown(cx: &mut TestAppContext) {
    let opened = open(cx, 3);
    update(cx, &opened, |_, _| {});
    cx.executor().advance_clock(Duration::from_millis(2500));
    assert!(opened.events.borrow().is_empty());
    cx.executor().advance_clock(Duration::from_millis(1000));
    assert_eq!(*opened.events.borrow(), [ThumbnailEvent::Close]);
    // Only once.
    cx.executor().advance_clock(Duration::from_secs(5));
    assert_eq!(opened.events.borrow().len(), 1);
}

#[gpui_kit::test]
fn the_countdown_waits_while_the_pointer_is_over_it(cx: &mut TestAppContext) {
    let opened = open(cx, 2);
    update(cx, &opened, |window, cx| window.hover("thumbnail", cx));
    cx.executor().advance_clock(Duration::from_secs(10));
    assert!(opened.events.borrow().is_empty());
    // The pointer leaves: two more seconds.
    update(cx, &opened, |window, cx| {
        pointer(window, point(px(-50.0), px(-50.0)), false, cx)
    });
    cx.executor().advance_clock(Duration::from_millis(2100));
    assert_eq!(*opened.events.borrow(), [ThumbnailEvent::Close]);
}

#[gpui_kit::test]
fn leaving_the_card_restarts_the_countdown_without_a_mouse_event(cx: &mut TestAppContext) {
    // The owner's case: hover, then leave. Windows sends the card no mouse
    // move once the pointer is outside, so the probe tells it.
    cx.update(gpui_kit::init);
    let over = Rc::new(std::cell::Cell::new(true));
    let probe = over.clone();
    let image = render_image(&scale_down(vec![200; 64 * 36 * 4], 64, 36, 64, 36));
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    let _handle = cx.open_window(size(px(252.0), px(147.0)), move |_, cx| {
        cx.subscribe_self(move |_, event: &ThumbnailEvent, _| sink.borrow_mut().push(*event))
            .detach();
        Thumbnail::new(image, 2, cx).with_pointer_probe(Box::new(move || probe.get()))
    });
    cx.executor().advance_clock(Duration::from_secs(10));
    assert!(events.borrow().is_empty());
    over.set(false);
    cx.executor().advance_clock(Duration::from_millis(3100));
    assert_eq!(*events.borrow(), [ThumbnailEvent::Close]);
}
