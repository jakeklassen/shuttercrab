//! Drives the real selection overlay in a headless GPUI window with a
//! synthetic frozen frame, using native pointer and keyboard events (PRD
//! §33, UI tests).
//!
//!   cargo test -p framecut --test ui
#![cfg(windows)]

use framecut::{
    overlay::{Mode, OverlayEvent, OverlayFrame, SelectionOverlay},
    selection::ScreenWindow,
};
use framecut_capture::PhysicalRect;
use gpui_kit::{
    App, AppContext as _, InputEvent as _, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels,
    Point, TestAppContext, Window, WindowHandle, point, px, size, test::TestWindowExt as _,
};
use std::{cell::RefCell, rc::Rc};

/// A frozen monitor of `logical` size at `scale`, and the events it emits.
struct Opened {
    handle: WindowHandle<SelectionOverlay>,
    events: Rc<RefCell<Vec<OverlayEvent>>>,
}

fn open(cx: &mut TestAppContext, logical: (f32, f32), scale: f32) -> Opened {
    open_with(cx, logical, scale, Vec::new(), false)
}

/// As [`open`], with windows on the frozen monitor (physical pixels).
fn open_with(
    cx: &mut TestAppContext,
    logical: (f32, f32),
    scale: f32,
    windows: Vec<ScreenWindow>,
    snap: bool,
) -> Opened {
    cx.update(gpui_kit::init);
    let (w, h) = ((logical.0 * scale) as u32, (logical.1 * scale) as u32);
    // A mid-grey frame, as a monitor would deliver it.
    let frame = OverlayFrame::from_bgra(w, h, scale, vec![128; (w * h * 4) as usize]);
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    let handle = cx.open_window(size(px(logical.0), px(logical.1)), move |window, cx| {
        cx.subscribe_self(move |_, event: &OverlayEvent, _| sink.borrow_mut().push(*event))
            .detach();
        SelectionOverlay::new(frame, window, cx).with_windows(windows, snap)
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

fn win(hwnd: isize, x: i32, y: i32, width: u32, height: u32) -> ScreenWindow {
    ScreenWindow {
        hwnd,
        bounds: PhysicalRect::new(x, y, width, height),
    }
}

fn hover(window: &mut Window, position: Point<Pixels>, cx: &mut App) {
    window.dispatch_event(
        MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Default::default(),
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
}

fn mode(cx: &mut TestAppContext, opened: &Opened) -> Mode {
    cx.update(|cx| opened.handle.read(cx).unwrap().mode())
}

#[gpui_kit::test]
fn space_switches_between_area_and_window(cx: &mut TestAppContext) {
    let opened = open(cx, (400.0, 300.0), 1.5);
    update(cx, &opened, |window, _| {
        assert!(label(window, "mode-hint").unwrap().starts_with("Drag"));
    });
    update(cx, &opened, |window, cx| window.press("space", cx));
    assert_eq!(mode(cx, &opened), Mode::Window);
    update(cx, &opened, |window, _| {
        assert!(
            label(window, "mode-hint")
                .unwrap()
                .starts_with("Click a window")
        );
    });
    update(cx, &opened, |window, cx| window.press("space", cx));
    assert_eq!(mode(cx, &opened), Mode::Area);
    assert!(opened.events.borrow().is_empty());
}

#[gpui_kit::test]
fn window_mode_highlights_the_window_under_the_pointer(cx: &mut TestAppContext) {
    // At 150%: a 300×150 window at physical (150, 150), in front of a
    // larger one; the monitor is 600×450.
    let windows = vec![win(1, 150, 150, 300, 150), win(2, 30, 30, 540, 390)];
    let opened = open_with(cx, (400.0, 300.0), 1.5, windows, false);
    update(cx, &opened, |window, cx| {
        window.press("space", cx);
        hover(window, point(px(150.0), px(150.0)), cx);
    });
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "window-target").as_deref(),
            Some("Window  300 × 150")
        );
    });
    update(cx, &opened, |window, cx| {
        hover(window, point(px(30.0), px(30.0)), cx)
    });
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "window-target").as_deref(),
            Some("Window  540 × 390")
        );
    });
    // Over the desktop: the whole display.
    update(cx, &opened, |window, cx| {
        hover(window, point(px(5.0), px(5.0)), cx)
    });
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "window-target").as_deref(),
            Some("Display  600 × 450")
        );
    });
}

#[gpui_kit::test]
fn clicking_a_window_captures_it_with_its_visible_part(cx: &mut TestAppContext) {
    // A window hanging off the bottom-right of the 600×450 monitor.
    let windows = vec![win(7, 500, 300, 300, 300)];
    let opened = open_with(cx, (400.0, 300.0), 1.5, windows, false);
    update(cx, &opened, |window, cx| {
        window.press("space", cx);
        window.click_at("overlay", point(px(350.0), px(210.0)), cx);
    });
    assert_eq!(
        *opened.events.borrow(),
        [OverlayEvent::Window {
            hwnd: 7,
            visible: PhysicalRect::new(500, 300, 100, 150)
        }]
    );
}

#[gpui_kit::test]
fn clicking_the_desktop_captures_the_display(cx: &mut TestAppContext) {
    let windows = vec![win(7, 300, 300, 100, 100)];
    let opened = open_with(cx, (400.0, 300.0), 1.5, windows, false);
    update(cx, &opened, |window, cx| {
        window.press("space", cx);
        window.click_at("overlay", point(px(20.0), px(20.0)), cx);
    });
    assert_eq!(*opened.events.borrow(), [OverlayEvent::Display]);
}

#[gpui_kit::test]
fn area_selections_snap_to_window_edges(cx: &mut TestAppContext) {
    // Window at physical (150, 150), 300×150: logical (100, 100) to (300, 200).
    let windows = || vec![win(1, 150, 150, 300, 150)];
    let near = (point(px(101.0), px(99.0)), point(px(298.0), px(198.0)));
    let snapping = open_with(cx, (400.0, 300.0), 1.5, windows(), true);
    update(cx, &snapping, |window, cx| window.drag(near.0, near.1, cx));
    assert_eq!(
        *snapping.events.borrow(),
        [OverlayEvent::Selected(PhysicalRect::new(
            150, 150, 300, 150
        ))]
    );
    let free = open_with(cx, (400.0, 300.0), 1.5, windows(), false);
    update(cx, &free, |window, cx| window.drag(near.0, near.1, cx));
    assert_eq!(
        *free.events.borrow(),
        [OverlayEvent::Selected(PhysicalRect::new(
            152, 149, 295, 148
        ))]
    );
}

fn press_at(window: &mut Window, position: Point<Pixels>, cx: &mut App) {
    window.dispatch_event(
        MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
}

fn drag_to(window: &mut Window, position: Point<Pixels>, cx: &mut App) {
    window.dispatch_event(
        MouseMoveEvent {
            position,
            pressed_button: Some(MouseButton::Left),
            modifiers: Default::default(),
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
}

#[gpui_kit::test]
fn a_snapped_edge_holds_until_dragged_well_away_and_shows_it(cx: &mut TestAppContext) {
    // At 150%, the window's right edge is at logical 300 (physical 450).
    // The drag starts free at logical (20, 20), physical (30, 30).
    let windows = vec![win(1, 150, 150, 300, 150)];
    let opened = open_with(cx, (400.0, 300.0), 1.5, windows, true);
    update(cx, &opened, |window, cx| {
        press_at(window, point(px(20.0), px(20.0)), cx);
        drag_to(window, point(px(250.0), px(150.0)), cx);
    });
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "dimensions").as_deref(), Some("345 × 195"));
        assert!(window.try_find("snapped-right").is_none());
    });
    // Within 10 logical pixels, the edge catches the selection…
    update(cx, &opened, |window, cx| {
        drag_to(window, point(px(292.0), px(150.0)), cx)
    });
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "dimensions").as_deref(), Some("420 × 195"));
        assert!(window.try_find("snapped-right").is_some());
        assert!(window.try_find("snapped-bottom").is_none());
    });
    // …holds it 20 logical pixels away…
    update(cx, &opened, |window, cx| {
        drag_to(window, point(px(280.0), px(150.0)), cx)
    });
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "dimensions").as_deref(), Some("420 × 195"));
    });
    // …and lets go beyond 24.
    update(cx, &opened, |window, cx| {
        drag_to(window, point(px(270.0), px(150.0)), cx)
    });
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "dimensions").as_deref(), Some("375 × 195"));
        assert!(window.try_find("snapped-right").is_none());
    });
}
