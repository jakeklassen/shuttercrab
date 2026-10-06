//! A short list to choose one item from, in a popup of its own: the ready
//! bar's microphones. The bar is a window only a button tall, so a list
//! cannot hang below it inside it.
//!
//! It takes the keyboard: Up and Down move, Enter or Space chooses, Escape
//! closes. Clicking an item chooses it; clicking elsewhere closes the list.

use crate::palette::{border, coral};
use gpui_kit::{
    ClickEvent, Context, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Render, Role, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, Window, div, prelude::FluentBuilder as _, px, rgb,
};

/// How the list closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChoiceMenuEvent {
    /// The item at this index was chosen.
    Chose(usize),
    Cancel,
}

/// The list's width, and each item's height, in logical pixels.
pub const CHOICE_MENU_WIDTH: f32 = 340.0;
const ITEM_HEIGHT: f32 = 32.0;
/// The padding inside the border, logical pixels.
const PADDING: f32 = 5.0;

/// The list's height for `items` items, logical pixels.
pub fn choice_menu_height(items: usize) -> f32 {
    items as f32 * ITEM_HEIGHT + 2.0 * PADDING + 2.0
}

pub struct ChoiceMenu {
    items: Vec<SharedString>,
    /// The item chosen before, marked.
    chosen: usize,
    highlighted: usize,
    /// Whether the window has had the keyboard: losing it then closes the
    /// list.
    was_active: bool,
    done: bool,
    focus: FocusHandle,
}

impl EventEmitter<ChoiceMenuEvent> for ChoiceMenu {}

impl ChoiceMenu {
    pub fn new(
        items: Vec<SharedString>,
        chosen: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.was_active = true;
            } else if this.was_active {
                this.finish(ChoiceMenuEvent::Cancel, cx);
            }
        })
        .detach();
        let chosen = chosen.min(items.len().saturating_sub(1));
        Self {
            items,
            chosen,
            highlighted: chosen,
            was_active: window.is_window_active(),
            done: false,
            focus,
        }
    }

    pub fn highlighted(&self) -> usize {
        self.highlighted
    }

    /// Report once; later keys and clicks are ignored.
    fn finish(&mut self, event: ChoiceMenuEvent, cx: &mut Context<Self>) {
        if !std::mem::replace(&mut self.done, true) {
            cx.emit(event);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let last = self.items.len().saturating_sub(1);
        match event.keystroke.key.as_str() {
            "up" => self.highlighted = self.highlighted.saturating_sub(1),
            "down" => self.highlighted = (self.highlighted + 1).min(last),
            "home" => self.highlighted = 0,
            "end" => self.highlighted = last,
            "enter" | "space" if !self.items.is_empty() => {
                self.finish(ChoiceMenuEvent::Chose(self.highlighted), cx)
            }
            "escape" => self.finish(ChoiceMenuEvent::Cancel, cx),
            _ => return,
        }
        cx.notify();
    }
}

impl Render for ChoiceMenu {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let items = self.items.iter().enumerate().map(|(i, label)| {
            div()
                .id(SharedString::from(format!("choice-{i}")))
                .role(Role::MenuItem)
                .aria_label(label.clone())
                .test_support()
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .h(px(ITEM_HEIGHT))
                .px_3()
                .rounded_md()
                .text_sm()
                .text_color(gpui_kit::white())
                .when(i == self.highlighted, |d| d.bg(rgb(0x383838)))
                .hover(|s| s.bg(rgb(0x383838)))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.highlighted = i;
                    this.finish(ChoiceMenuEvent::Chose(i), cx);
                }))
                .when(i == self.chosen, |d| {
                    d.child(
                        div()
                            .absolute()
                            .left(px(0.))
                            .top(px(9.))
                            .bottom(px(9.))
                            .w(px(3.))
                            .rounded_full()
                            .bg(coral()),
                    )
                })
                .child(div().min_w_0().truncate().child(label.clone()))
        });
        div()
            .id("choice-menu")
            .role(Role::Menu)
            .test_support()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .flex()
            .flex_col()
            .p(px(PADDING))
            .bg(rgb(0x2C2C2C))
            .border_1()
            .border_color(border())
            .children(items)
    }
}
