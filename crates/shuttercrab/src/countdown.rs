//! The countdown before recording starts (PRD §7.7): a large number over
//! the chosen area, counting down a second at a time. Enter or Space starts
//! at once, Escape cancels. It is excluded from capture and gone before the
//! first frame.

use crate::palette::{border, muted, surface};
use gpui_kit::{
    Context, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Render, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, div, px,
};
use std::time::Duration;

/// How the countdown ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CountdownEvent {
    /// Start recording: the count reached zero, or the user did not wait.
    Go,
    Cancel,
}

/// The countdown's size in logical pixels.
pub const COUNTDOWN_WIDTH: f32 = 220.0;
pub const COUNTDOWN_HEIGHT: f32 = 180.0;

const SECOND: Duration = Duration::from_secs(1);

pub struct Countdown {
    left: u32,
    done: bool,
    focus: FocusHandle,
}

impl EventEmitter<CountdownEvent> for Countdown {}

impl Countdown {
    pub fn new(seconds: u32, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SECOND).await;
                let done = this.update(cx, |this, cx| {
                    this.left = this.left.saturating_sub(1);
                    if this.left == 0 {
                        this.finish(CountdownEvent::Go, cx);
                    }
                    cx.notify();
                    this.done
                });
                if done.unwrap_or(true) {
                    break;
                }
            }
        })
        .detach();
        Self {
            left: seconds.max(1),
            done: false,
            focus,
        }
    }

    pub fn left(&self) -> u32 {
        self.left
    }

    /// Cancel, as Escape does: the window to record was closed.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.finish(CountdownEvent::Cancel, cx);
    }

    /// Report once; later keys and ticks are ignored.
    fn finish(&mut self, event: CountdownEvent, cx: &mut Context<Self>) {
        if !std::mem::replace(&mut self.done, true) {
            cx.emit(event);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => self.finish(CountdownEvent::Cancel, cx),
            "enter" | "space" => self.finish(CountdownEvent::Go, cx),
            _ => {}
        }
    }
}

impl Render for Countdown {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let number: SharedString = self.left.to_string().into();
        div()
            .id("countdown")
            .role(Role::Status)
            .aria_label(format!("Recording in {}", self.left))
            .test_support()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .bg(surface())
            .border_1()
            .border_color(border())
            .child(
                div()
                    .text_size(px(88.))
                    .line_height(px(96.))
                    .text_color(gpui_kit::white())
                    .child(number),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted())
                    .child("Enter starts now · Esc cancels"),
            )
    }
}
