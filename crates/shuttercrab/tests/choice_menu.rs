//! Drives the choice list (the ready bar's microphones) in a headless GPUI
//! window with native pointer and keyboard events.
//!
//!   cargo test -p shuttercrab --test choice_menu
#![cfg(windows)]

use gpui_kit::{
    AppContext as _, SharedString, TestAppContext, WindowHandle, px, size, test::TestWindowExt as _,
};
use shuttercrab::choice_menu::{
    CHOICE_MENU_WIDTH, ChoiceMenu, ChoiceMenuEvent, choice_menu_height,
};
use std::{cell::RefCell, rc::Rc};

struct Opened {
    handle: WindowHandle<ChoiceMenu>,
    events: Rc<RefCell<Vec<ChoiceMenuEvent>>>,
}

/// Three items, the second chosen before.
fn open(cx: &mut TestAppContext) -> Opened {
    cx.update(gpui_kit::init);
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    let items: Vec<SharedString> = vec!["Windows' default".into(), "Yeti".into(), "C920".into()];
    let handle = cx.open_window(
        size(px(CHOICE_MENU_WIDTH), px(choice_menu_height(items.len()))),
        move |window, cx| {
            cx.subscribe_self(move |_, event: &ChoiceMenuEvent, _| sink.borrow_mut().push(*event))
                .detach();
            ChoiceMenu::new(items, 1, window, cx)
        },
    );
    cx.run_until_parked();
    Opened { handle, events }
}

fn press(cx: &mut TestAppContext, opened: &Opened, keys: &[&str]) {
    cx.update_window(opened.handle.into(), |_, window, cx| {
        window.render_frame(cx);
        for key in keys {
            window.press(key, cx);
        }
    })
    .unwrap();
}

fn highlighted(cx: &mut TestAppContext, opened: &Opened) -> usize {
    cx.update(|cx| opened.handle.read(cx).unwrap().highlighted())
}

#[gpui_kit::test]
fn starts_on_the_chosen_item_and_moves_by_key(cx: &mut TestAppContext) {
    let opened = open(cx);
    assert_eq!(highlighted(cx, &opened), 1);
    press(cx, &opened, &["down", "down"]);
    assert_eq!(highlighted(cx, &opened), 2);
    press(cx, &opened, &["up", "up", "up"]);
    assert_eq!(highlighted(cx, &opened), 0);
    press(cx, &opened, &["end", "enter", "escape"]);
    // Only the first answer counts.
    assert_eq!(*opened.events.borrow(), [ChoiceMenuEvent::Chose(2)]);
}

#[gpui_kit::test]
fn escape_cancels(cx: &mut TestAppContext) {
    let opened = open(cx);
    press(cx, &opened, &["escape"]);
    assert_eq!(*opened.events.borrow(), [ChoiceMenuEvent::Cancel]);
}

#[gpui_kit::test]
fn clicking_an_item_chooses_it(cx: &mut TestAppContext) {
    let opened = open(cx);
    cx.update_window(opened.handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("choice-0", cx);
    })
    .unwrap();
    assert_eq!(*opened.events.borrow(), [ChoiceMenuEvent::Chose(0)]);
}
