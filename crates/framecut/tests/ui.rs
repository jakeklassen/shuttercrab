//! Drives the real selection overlay in a headless GPUI window with a
//! synthetic frozen frame, using native pointer and keyboard events (PRD
//! §33, UI tests).
//!
//!   cargo test -p framecut --test ui
#![cfg(windows)]

use framecut::overlay::{OverlayEvent, OverlayFrame, SelectionOverlay};
use framecut_capture::PhysicalRect;
use gpui_kit::{
    App, AppContext as _, TestAppContext, Window, WindowHandle, point, px, size,
    test::TestWindowExt as _,
};
use std::{cell::RefCell, rc::Rc};

/// A frozen monitor of `logical` size at `scale`, and the events it emits.
struct Opened {
    handle: WindowHandle<SelectionOverlay>,
    events: Rc<RefCell<Vec<OverlayEvent>>>,
}

fn open(cx: &mut TestAppContext, logical: (f32, f32), scale: f32) -> Opened {
    cx.update(gpui_kit::init);
    let (w, h) = ((logical.0 * scale) as u32, (logical.1 * scale) as u32);
    // A mid-grey frame, as a monitor would deliver it.
    let frame = OverlayFrame::from_bgra(w, h, scale, vec![128; (w * h * 4) as usize]);
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    let handle = cx.open_window(size(px(logical.0), px(logical.1)), move |window, cx| {
        cx.subscribe_self(move |_, event: &OverlayEvent, _| sink.borrow_mut().push(*event))
            .detach();
        SelectionOverlay::new(frame, window, cx)
    });
    Opened { handle, events }
}

fn update(cx: &mut TestAppContext, opened: &Opened, f: impl FnOnce(&mut Window, &mut App)) {
    // Through the untyped handle, so the view is not borrowed while the
    // window renders it.
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

#[gpui_kit::test]
fn dragging_selects_physical_pixels_and_shows_their_dimensions(cx: &mut TestAppContext) {
    let opened = open(cx, (400.0, 300.0), 1.5);
    update(cx, &opened, |window, cx| {
        assert!(window.try_find("selection").is_none());
        window.drag(point(px(20.0), px(30.0)), point(px(120.0), px(80.0)), cx);
    });
    update(cx, &opened, |window, _| {
        // 100×50 logical at 150% is 150×75 physical.
        assert_eq!(label(window, "dimensions").as_deref(), Some("150 × 75"));
        assert_eq!(label(window, "selection").as_deref(), Some("150 × 75"));
    });
    assert_eq!(
        *opened.events.borrow(),
        [OverlayEvent::Selected(PhysicalRect::new(30, 45, 150, 75))]
    );
}

#[gpui_kit::test]
fn dimensions_are_physical_at_every_scale_factor(cx: &mut TestAppContext) {
    // Corners snap to the nearest physical pixel: at 125%, logical 10 is
    // physical 12.5 and rounds to 13, so the 50-logical-pixel height is 62.
    for (scale, expected) in [
        (1.0, "100 × 50"),
        (1.25, "125 × 62"),
        (1.75, "175 × 87"),
        (2.0, "200 × 100"),
    ] {
        let opened = open(cx, (400.0, 300.0), scale);
        update(cx, &opened, |window, cx| {
            window.drag(point(px(10.0), px(10.0)), point(px(110.0), px(60.0)), cx);
        });
        update(cx, &opened, |window, _| {
            assert_eq!(
                label(window, "dimensions").as_deref(),
                Some(expected),
                "scale {scale}"
            );
        });
    }
}

#[gpui_kit::test]
fn a_drag_in_any_direction_selects_the_same_region(cx: &mut TestAppContext) {
    let opened = open(cx, (400.0, 300.0), 2.0);
    update(cx, &opened, |window, cx| {
        window.drag(point(px(120.0), px(80.0)), point(px(20.0), px(30.0)), cx);
    });
    assert_eq!(
        *opened.events.borrow(),
        [OverlayEvent::Selected(PhysicalRect::new(40, 60, 200, 100))]
    );
}

#[gpui_kit::test]
fn escape_cancels(cx: &mut TestAppContext) {
    let opened = open(cx, (400.0, 300.0), 1.5);
    update(cx, &opened, |window, cx| {
        window.press("escape", cx);
    });
    assert_eq!(*opened.events.borrow(), [OverlayEvent::Cancelled]);
}

#[gpui_kit::test]
fn right_click_cancels(cx: &mut TestAppContext) {
    let opened = open(cx, (400.0, 300.0), 1.5);
    update(cx, &opened, |window, cx| {
        window.right_click("overlay", cx);
    });
    assert_eq!(*opened.events.borrow(), [OverlayEvent::Cancelled]);
}

#[gpui_kit::test]
fn a_click_without_a_drag_selects_nothing(cx: &mut TestAppContext) {
    let opened = open(cx, (400.0, 300.0), 1.5);
    update(cx, &opened, |window, cx| {
        window.click("overlay", cx);
    });
    update(cx, &opened, |window, _| {
        assert!(window.try_find("selection").is_none());
        assert!(window.try_find("dimensions").is_none());
    });
    assert!(opened.events.borrow().is_empty());
}

#[gpui_kit::test]
fn the_overlay_takes_focus_so_escape_works_at_once(cx: &mut TestAppContext) {
    let opened = open(cx, (400.0, 300.0), 1.0);
    update(cx, &opened, |window, _| {
        assert_eq!(window.find("overlay").focused(), Some(true));
    });
}
