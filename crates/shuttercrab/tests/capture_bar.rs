//! Drives the real Capture Bar in a headless GPUI window with native
//! pointer and keyboard events (PRD §7.5, §33).
//!
//!   cargo test -p shuttercrab --test capture_bar

use gpui_kit::{
    App, AppContext as _, TestAppContext, VisualTestContext, Window, WindowHandle, px, size,
    test::TestWindowExt as _,
};
use shuttercrab::{
    capture_bar::{BAR_HEIGHT, BAR_WIDTH, CaptureBar, CaptureBarEvent},
    capture_choice::{CaptureMode, CaptureTarget},
};
use std::{cell::RefCell, rc::Rc};

struct Opened {
    handle: WindowHandle<CaptureBar>,
    events: Rc<RefCell<Vec<CaptureBarEvent>>>,
}

fn open(cx: &mut TestAppContext, last: CaptureTarget) -> Opened {
    open_in(cx, CaptureMode::Screenshot, last)
}

fn open_in(cx: &mut TestAppContext, mode: CaptureMode, last: CaptureTarget) -> Opened {
    cx.update(gpui_kit::init);
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    let handle = cx.open_window(size(px(BAR_WIDTH), px(BAR_HEIGHT)), move |window, cx| {
        cx.subscribe_self(move |_, event: &CaptureBarEvent, _| sink.borrow_mut().push(*event))
            .detach();
        CaptureBar::new(last, window, cx).with_mode(mode)
    });
    Opened { handle, events }
}

fn update(cx: &mut TestAppContext, opened: &Opened, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(opened.handle.into(), |_, window, cx| {
        window.render_frame(cx);
        f(window, cx);
    })
    .unwrap();
}

fn selected(cx: &mut TestAppContext, opened: &Opened) -> CaptureTarget {
    cx.update(|cx| opened.handle.read(cx).unwrap().selected())
}

#[gpui_kit::test]
fn opens_on_the_last_target_and_enter_takes_it(cx: &mut TestAppContext) {
    let opened = open(cx, CaptureTarget::Window);
    assert_eq!(selected(cx, &opened), CaptureTarget::Window);
    update(cx, &opened, |window, _| {
        for id in [
            "target-area",
            "target-window",
            "target-display",
            "target-freeform",
            "mode-screenshot",
        ] {
            assert!(window.try_find(id).is_some(), "{id}");
        }
    });
    update(cx, &opened, |window, cx| window.press("enter", cx));
    assert_eq!(
        *opened.events.borrow(),
        [CaptureBarEvent::Chosen(
            CaptureMode::Screenshot,
            CaptureTarget::Window
        )]
    );
}

#[gpui_kit::test]
fn arrows_and_tab_move_the_selection_and_wrap(cx: &mut TestAppContext) {
    let opened = open(cx, CaptureTarget::Area);
    update(cx, &opened, |window, cx| window.press("right", cx));
    assert_eq!(selected(cx, &opened), CaptureTarget::Window);
    update(cx, &opened, |window, cx| window.press("tab", cx));
    assert_eq!(selected(cx, &opened), CaptureTarget::Display);
    update(cx, &opened, |window, cx| window.press("right", cx));
    assert_eq!(selected(cx, &opened), CaptureTarget::Freeform);
    update(cx, &opened, |window, cx| window.press("right", cx));
    assert_eq!(selected(cx, &opened), CaptureTarget::Area);
    update(cx, &opened, |window, cx| window.press("left", cx));
    assert_eq!(selected(cx, &opened), CaptureTarget::Freeform);
    update(cx, &opened, |window, cx| window.press("shift-tab", cx));
    assert_eq!(selected(cx, &opened), CaptureTarget::Display);
    assert!(opened.events.borrow().is_empty());
}

#[gpui_kit::test]
fn letters_choose_directly(cx: &mut TestAppContext) {
    for (key, target) in [
        ("a", CaptureTarget::Area),
        ("w", CaptureTarget::Window),
        ("d", CaptureTarget::Display),
        ("f", CaptureTarget::Freeform),
    ] {
        let opened = open(cx, CaptureTarget::Area);
        update(cx, &opened, |window, cx| window.press(key, cx));
        assert_eq!(
            *opened.events.borrow(),
            [CaptureBarEvent::Chosen(CaptureMode::Screenshot, target)]
        );
    }
}

#[gpui_kit::test]
fn clicking_a_target_chooses_it(cx: &mut TestAppContext) {
    let opened = open(cx, CaptureTarget::Area);
    update(cx, &opened, |window, cx| window.click("target-display", cx));
    assert_eq!(
        *opened.events.borrow(),
        [CaptureBarEvent::Chosen(
            CaptureMode::Screenshot,
            CaptureTarget::Display
        )]
    );
}

#[gpui_kit::test]
fn escape_dismisses_and_nothing_follows(cx: &mut TestAppContext) {
    let opened = open(cx, CaptureTarget::Area);
    update(cx, &opened, |window, cx| {
        window.press("escape", cx);
        window.press("enter", cx);
    });
    assert_eq!(*opened.events.borrow(), [CaptureBarEvent::Dismissed]);
}

#[gpui_kit::test]
fn clicking_elsewhere_dismisses(cx: &mut TestAppContext) {
    let opened = open(cx, CaptureTarget::Area);
    update(cx, &opened, |window, _| window.activate_window());
    cx.run_until_parked();
    assert!(opened.events.borrow().is_empty());
    VisualTestContext::from_window(opened.handle.into(), cx).deactivate_window();
    assert_eq!(*opened.events.borrow(), [CaptureBarEvent::Dismissed]);
}

#[gpui_kit::test]
fn r_and_s_switch_modes_and_record_skips_freeform(cx: &mut TestAppContext) {
    let opened = open(cx, CaptureTarget::Window);
    let mode = |cx: &mut TestAppContext| cx.update(|cx| opened.handle.read(cx).unwrap().mode());
    update(cx, &opened, |window, _| {
        assert!(window.try_find("mode-record").is_some());
    });
    // Recordings are of an area, a window or a display: Freeform is
    // passed over.
    update(cx, &opened, |window, cx| window.press("r", cx));
    assert_eq!(mode(cx), CaptureMode::Record);
    assert_eq!(selected(cx, &opened), CaptureTarget::Window);
    update(cx, &opened, |window, cx| window.press("right", cx));
    assert_eq!(selected(cx, &opened), CaptureTarget::Display);
    update(cx, &opened, |window, cx| window.press("right", cx));
    assert_eq!(selected(cx, &opened), CaptureTarget::Area);
    update(cx, &opened, |window, cx| {
        window.press("f", cx);
        window.click("target-freeform", cx);
    });
    assert!(opened.events.borrow().is_empty());
    update(cx, &opened, |window, cx| window.press("s", cx));
    assert_eq!(mode(cx), CaptureMode::Screenshot);
    update(cx, &opened, |window, cx| window.press("f", cx));
    assert_eq!(
        *opened.events.borrow(),
        [CaptureBarEvent::Chosen(
            CaptureMode::Screenshot,
            CaptureTarget::Freeform
        )]
    );
}

#[gpui_kit::test]
fn opens_in_the_last_mode_and_records_the_choice(cx: &mut TestAppContext) {
    let opened = open_in(cx, CaptureMode::Record, CaptureTarget::Freeform);
    assert_eq!(selected(cx, &opened), CaptureTarget::Area);
    update(cx, &opened, |window, cx| window.press("enter", cx));
    assert_eq!(
        *opened.events.borrow(),
        [CaptureBarEvent::Chosen(
            CaptureMode::Record,
            CaptureTarget::Area
        )]
    );
}

#[gpui_kit::test]
fn a_window_can_be_recorded(cx: &mut TestAppContext) {
    let opened = open_in(cx, CaptureMode::Record, CaptureTarget::Area);
    update(cx, &opened, |window, cx| window.press("w", cx));
    assert_eq!(
        *opened.events.borrow(),
        [CaptureBarEvent::Chosen(
            CaptureMode::Record,
            CaptureTarget::Window
        )]
    );
}
#[gpui_kit::test]
fn clicking_record_then_display_records_the_display(cx: &mut TestAppContext) {
    let opened = open(cx, CaptureTarget::Area);
    update(cx, &opened, |window, cx| window.click("mode-record", cx));
    update(cx, &opened, |window, cx| window.click("target-display", cx));
    assert_eq!(
        *opened.events.borrow(),
        [CaptureBarEvent::Chosen(
            CaptureMode::Record,
            CaptureTarget::Display
        )]
    );
}
