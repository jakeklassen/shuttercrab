//! The recording controls (PRD §16): a small bar near the recorded area
//! with the elapsed time, Pause/Resume, Stop, and a ⋯ menu with Restart
//! and Discard.
//!
//! The bar never takes the keyboard by itself: the app being recorded
//! keeps it, so typing is never mistaken for a command. Its hints show the
//! global chords, which work from anywhere while recording. Once the bar
//! is clicked it has the keyboard, and its hints switch to letters: P or
//! Space pauses, S stops, and M opens the menu, where R restarts, D
//! discards and Escape goes back; clicking elsewhere hands the keyboard
//! back. The two destructive actions always take two steps. The bar only
//! reports what was asked; the app does it.

use crate::{
    capture_bar::{muted, recording, surface, tile},
    recording::Clock,
};
use gpui_kit::{
    AnyElement, Context, EventEmitter, FocusHandle, Hsla, InteractiveElement as _, IntoElement,
    KeyDownEvent, MouseButton, MouseUpEvent, ParentElement as _, Render, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, assets::IconName,
    component::Icon, div, prelude::FluentBuilder as _, px, rgb,
};
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

/// What the user asked of the recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordBarEvent {
    TogglePause,
    Stop,
    Restart,
    Discard,
}

/// The global chords that work while recording, as the settings spell
/// them (`Ctrl+Alt+P`).
#[derive(Clone, Debug)]
pub struct RecordKeys {
    pub pause: String,
    pub stop: String,
}

/// The bar's size in logical pixels.
pub const RECORD_BAR_WIDTH: f32 = 440.0;
pub const RECORD_BAR_HEIGHT: f32 = 48.0;

/// How often the time is redrawn.
const TICK: Duration = Duration::from_millis(250);

/// The amber of a paused recording.
fn paused() -> Hsla {
    rgb(0xF5A524).into()
}

pub struct RecordBar {
    /// The recording's clock, kept by the app.
    clock: Rc<Cell<Clock>>,
    /// Showing Restart and Discard instead of Pause and Stop.
    menu: bool,
    /// Whether the bar has the keyboard, so the letters work.
    active: bool,
    keys: RecordKeys,
    focus: FocusHandle,
}

impl EventEmitter<RecordBarEvent> for RecordBar {}

impl RecordBar {
    pub fn new(
        clock: Rc<Cell<Clock>>,
        keys: RecordKeys,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        // Keys arrive once the bar is clicked; it never takes them itself.
        window.focus(&focus, cx);
        cx.observe_window_activation(window, |this, window, cx| {
            this.active = window.is_window_active();
            // Handing the keyboard back closes the menu, whose keys it had.
            if !this.active {
                this.menu = false;
            }
            cx.notify();
        })
        .detach();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            clock,
            menu: false,
            active: window.is_window_active(),
            keys,
            focus,
        }
    }

    pub fn menu_open(&self) -> bool {
        self.menu
    }

    /// The key hint for a button: its letter while the bar has the
    /// keyboard, otherwise its global chord, if it has one.
    fn hint(&self, letter: &'static str, chord: Option<&str>) -> SharedString {
        match (self.active, chord) {
            (true, _) => letter.into(),
            (false, Some(chord)) => chord.to_string().into(),
            (false, None) => SharedString::default(),
        }
    }

    /// The elapsed time as shown.
    pub fn time(&self) -> String {
        crate::recording::clock(self.clock.get().elapsed(Instant::now()))
    }

    fn ask(&mut self, event: RecordBarEvent, cx: &mut Context<Self>) {
        self.menu = false;
        cx.emit(event);
        cx.notify();
    }

    fn set_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.menu = open;
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match (self.menu, event.keystroke.key.as_str()) {
            (false, "p" | "space") => self.ask(RecordBarEvent::TogglePause, cx),
            (false, "s") => self.ask(RecordBarEvent::Stop, cx),
            (false, "m") => self.set_menu(true, cx),
            (true, "r") => self.ask(RecordBarEvent::Restart, cx),
            (true, "d") => self.ask(RecordBarEvent::Discard, cx),
            (true, "escape" | "m") => self.set_menu(false, cx),
            _ => {}
        }
    }

    /// A button: an icon, a label unless `label` is empty, and its key.
    fn button(
        &self,
        id: &'static str,
        icon: IconName,
        label: &'static str,
        key: SharedString,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let name: SharedString = if label.is_empty() { "More" } else { label }.into();
        let key_id = SharedString::from(format!("{id}-key"));
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(name)
            .test_support()
            .flex()
            .items_center()
            .gap_1p5()
            .h(px(32.))
            .px_2()
            .rounded_md()
            .bg(tile())
            .hover(|s| s.bg(rgb(0x353535)))
            .cursor_pointer()
            .text_sm()
            .text_color(gpui_kit::white())
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseUpEvent, _, cx| action(this, cx)),
            )
            .child(Icon::new(icon).size(px(14.)))
            .when(!label.is_empty(), |d| d.child(label))
            .when(!key.is_empty(), |d| {
                d.child(
                    div()
                        .id(key_id)
                        .role(Role::Status)
                        .aria_label(key.clone())
                        .test_support()
                        .text_xs()
                        .text_color(muted())
                        .child(key),
                )
            })
            .into_any_element()
    }
}

impl Render for RecordBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_paused = self.clock.get().is_paused();
        let status: SharedString = if is_paused {
            format!("Paused at {}", self.time())
        } else {
            format!("Recording {}", self.time())
        }
        .into();
        let buttons = if self.menu {
            vec![
                self.button(
                    "record-restart",
                    IconName::RotateCcw,
                    "Restart",
                    self.hint("R", None),
                    cx,
                    |this, cx| this.ask(RecordBarEvent::Restart, cx),
                ),
                self.button(
                    "record-discard",
                    IconName::Trash,
                    "Discard",
                    self.hint("D", None),
                    cx,
                    |this, cx| this.ask(RecordBarEvent::Discard, cx),
                ),
                self.button(
                    "record-back",
                    IconName::X,
                    "",
                    self.hint("Esc", None),
                    cx,
                    |this, cx| this.set_menu(false, cx),
                ),
            ]
        } else {
            let (icon, label) = if is_paused {
                (IconName::Play, "Resume")
            } else {
                (IconName::Pause, "Pause")
            };
            let pause = self.hint("P", Some(&self.keys.pause));
            let stop = self.hint("S", Some(&self.keys.stop));
            let more = self.hint("M", None);
            vec![
                self.button("record-pause", icon, label, pause, cx, |this, cx| {
                    this.ask(RecordBarEvent::TogglePause, cx)
                }),
                self.button(
                    "record-stop",
                    IconName::Square,
                    "Stop",
                    stop,
                    cx,
                    |this, cx| this.ask(RecordBarEvent::Stop, cx),
                ),
                self.button(
                    "record-more",
                    IconName::Ellipsis,
                    "",
                    more,
                    cx,
                    |this, cx| this.set_menu(true, cx),
                ),
            ]
        };
        div()
            .id("record-bar")
            .role(Role::Toolbar)
            .aria_label("Recording controls")
            .test_support()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .bg(surface())
            .border_1()
            .border_color(rgb(0x3A3A3A))
            .child(div().size(px(10.)).rounded_full().bg(if is_paused {
                paused()
            } else {
                recording()
            }))
            .child(
                div()
                    .id("record-time")
                    .role(Role::Status)
                    .aria_label(status)
                    .test_support()
                    .flex_1()
                    .text_sm()
                    .text_color(if is_paused {
                        paused()
                    } else {
                        gpui_kit::white()
                    })
                    .child(self.time()),
            )
            .children(buttons)
    }
}
