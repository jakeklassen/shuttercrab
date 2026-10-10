//! Drives the real post-capture thumbnail in a headless GPUI window with
//! native pointer events (PRD §7.6, §33).
//!
//!   cargo test -p shuttercrab --test thumbnail

use gpui_kit::{
    App, AppContext as _, InputEvent as _, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, TestAppContext, Window, WindowHandle, point, px, size,
    test::TestWindowExt as _,
};
use shuttercrab::thumbnail::{Thumbnail, ThumbnailEvent, render_image, scale_down};
use std::{cell::RefCell, rc::Rc, time::Duration};

struct Opened {
    handle: WindowHandle<Thumbnail>,
    events: Rc<RefCell<Vec<ThumbnailEvent>>>,
}

fn open(cx: &mut TestAppContext, seconds: u32) -> Opened {
    cx.update(gpui_kit::init);
    let image = render_image(&scale_down(&[200; 64 * 36 * 4], 64, 36, 64, 36));
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    let handle = cx.open_window(size(px(252.0), px(147.0)), move |_, cx| {
        cx.subscribe_self(move |_, event: &ThumbnailEvent, _| sink.borrow_mut().push(*event))
            .detach();
        Thumbnail::new(image, seconds, cx)
    });
    Opened { handle, events }
}

/// The pointer moves onto or off the card. In the app, GPUI reports this
/// as the window's hover state, which a headless window never changes.
fn over(cx: &mut TestAppContext, opened: &Opened, over: bool) {
    opened
        .handle
        .update(cx, |card, _, cx| card.set_pointer_over(over, cx))
        .unwrap();
    update(cx, opened, |_, _| {});
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
    over(cx, &opened, true);
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
    cx.executor().advance_clock(Duration::from_millis(1500));
    over(cx, &opened, true);
    cx.executor().advance_clock(Duration::from_secs(10));
    assert!(opened.events.borrow().is_empty());
    // The pointer leaves: the countdown carries on where it stopped, with
    // half a second left.
    over(cx, &opened, false);
    cx.executor().advance_clock(Duration::from_millis(400));
    assert!(opened.events.borrow().is_empty());
    cx.executor().advance_clock(Duration::from_millis(200));
    assert_eq!(*opened.events.borrow(), [ThumbnailEvent::Close]);
}

#[gpui_kit::test]
fn a_click_stops_the_countdown(cx: &mut TestAppContext) {
    let opened = open(cx, 2);
    update(cx, &opened, |window, cx| window.click("thumbnail", cx));
    cx.executor().advance_clock(Duration::from_secs(5));
    assert_eq!(*opened.events.borrow(), [ThumbnailEvent::Open]);
}
