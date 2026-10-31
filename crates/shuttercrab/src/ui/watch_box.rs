//! The Watch box (issue #66): a small window to paste a stream's link
//! into. Enter opens the viewer, Escape closes the box. A link already on
//! the clipboard is filled in, so most often Enter is all it takes.

use crate::ui::palette::{border, muted, recording, surface};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Render, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
    assets::IconName,
    component::{
        Icon,
        input::{Input, InputEvent, InputState},
    },
    div, px,
};

/// The box's size in logical pixels.
pub const WATCH_BOX_WIDTH: f32 = 560.0;
pub const WATCH_BOX_HEIGHT: f32 = 128.0;

/// What the box asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchBoxEvent {
    /// Watch the stream at this link.
    Watch(String),
    Cancel,
}

pub struct WatchBox {
    input: Entity<InputState>,
    /// Why the last Enter did nothing.
    error: Option<SharedString>,
    focus: FocusHandle,
}

impl EventEmitter<WatchBoxEvent> for WatchBox {}

impl WatchBox {
    /// A box holding `link`, if one was on the clipboard.
    pub fn new(link: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            let state = InputState::new(window, cx).placeholder("shuttercrab://watch/…");
            match link {
                Some(link) => state.default_value(link),
                None => state,
            }
        });
        cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            match event {
                InputEvent::PressEnter { .. } => this.submit(cx),
                // A new link clears an old complaint.
                InputEvent::Change if this.error.is_some() => {
                    this.error = None;
                    cx.notify();
                }
                _ => {}
            }
        })
        .detach();
        input.update(cx, |input, cx| input.focus(window, cx));
        Self {
            input,
            error: None,
            focus: cx.focus_handle(),
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value();
        if crate::helpers::viewer::is_link(&text) {
            cx.emit(WatchBoxEvent::Watch(text.trim().to_string()));
        } else {
            self.error = Some("That is not a stream link: they start shuttercrab://watch/".into());
            cx.notify();
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.emit(WatchBoxEvent::Cancel);
        }
    }
}

impl Render for WatchBox {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (hint, color) = match &self.error {
            Some(error) => (error.clone(), recording()),
            None => ("Enter watches · Esc closes".into(), muted()),
        };
        div()
            .id("watch-box")
            .role(Role::Dialog)
            .aria_label("Watch a stream")
            .test_support()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .bg(surface())
            .border_1()
            .border_color(border())
            .text_color(gpui_kit::white())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .child(Icon::new(IconName::MonitorPlay).size(px(16.)))
                    .child("Watch a stream: paste its link"),
            )
            .child(Input::new(&self.input))
            .child(
                div()
                    .id("watch-box-hint")
                    .role(Role::Status)
                    .aria_label(hint.clone())
                    .test_support()
                    .text_xs()
                    .text_color(color)
                    .child(hint),
            )
    }
}
