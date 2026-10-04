//! The recording controls (PRD §16): a small bar near the recorded area
//! with the elapsed time and Pause/Resume, Stop, Restart and Discard, all
//! in view (the owner found a ⋯ menu for two actions only slowed them
//! down).
//!
//! The bar never takes the keyboard by itself: the app being recorded
//! keeps it, so typing is never mistaken for a command. Its hints show the
//! global chords, which work from anywhere while recording. Once the bar
//! is clicked it has the keyboard, and its hints switch to letters: P or
//! Space pauses, S stops, N restarts, D discards.
//!
//! Throwing a take away is guarded, as the settings say: either the bar
//! asks first (Enter or the same chord again confirms, Escape keeps the
//! take), or the action happens at once and can be undone for a few
//! seconds; a discarded take counts down with Undo (Z) and Discard now
//! (Enter, or the discard chord again). The bar only reports what was
//! asked; the app does it and tells the bar what to show.

use crate::{
    palette::{border, hover, muted, paused, recording, surface, tile},
    recording::{Clock, clock},
};
use gpui_kit::{
    AnyElement, Context, EventEmitter, FocusHandle, Hsla, InteractiveElement as _, IntoElement,
    KeyDownEvent, MouseButton, MouseUpEvent, ParentElement as _, Render, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, assets::IconName,
    component::Icon, div, prelude::FluentBuilder as _, px,
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
    /// Yes to the question the bar is asking.
    Confirm,
    /// No to it: keep the take.
    Cancel,
    Undo,
}

/// An action that throws a take away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destructive {
    Discard,
    Restart,
}

/// What the bar shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarMode {
    /// The time and the four actions.
    Controls,
    /// Asking before `Destructive` throws away a take `length` long.
    Confirm(Destructive, Duration),
    /// The take is discarded, and deleted at `until` unless undone.
    Discarded { until: Instant },
}

/// The global chords that work while recording, as the settings spell
/// them (`Ctrl+Alt+P`).
#[derive(Clone, Debug)]
pub struct RecordKeys {
    pub pause: String,
    pub stop: String,
    pub restart: String,
    pub discard: String,
    pub undo: String,
}

/// The bar's size in logical pixels.
pub const RECORD_BAR_WIDTH: f32 = 860.0;
pub const RECORD_BAR_HEIGHT: f32 = 48.0;

/// The time is redrawn once a second, just after the clock reaches the
/// next whole second (paused, the countdowns still move each second).
const SECOND: Duration = Duration::from_secs(1);

/// How long until the bar next needs redrawing: just past the recorded
/// time's next whole second while it runs, a second while paused.
pub fn next_redraw(clock: Clock, now: Instant) -> Duration {
    if clock.is_paused() {
        return SECOND;
    }
    let into = clock.elapsed(now).subsec_nanos();
    SECOND - Duration::from_nanos(into.into()) + Duration::from_millis(5)
}

pub struct RecordBar {
    /// The recording's clock, kept by the app.
    clock: Rc<Cell<Clock>>,
    mode: BarMode,
    /// A restart kept the previous take until this moment; Undo keeps it
    /// for good.
    previous_until: Option<Instant>,
    /// A short message in place of the time, until it expires.
    notice: Option<(SharedString, Instant)>,
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
            cx.notify();
        })
        .detach();
        let first = next_redraw(clock.get(), Instant::now());
        cx.spawn(async move |this, cx| {
            let mut wait = first;
            loop {
                cx.background_executor().timer(wait).await;
                let redrawn = this.update(cx, |this, cx| {
                    cx.notify();
                    next_redraw(this.clock.get(), Instant::now())
                });
                match redrawn {
                    Ok(next) => wait = next,
                    Err(_) => break,
                }
            }
        })
        .detach();
        Self {
            clock,
            mode: BarMode::Controls,
            previous_until: None,
            notice: None,
            active: window.is_window_active(),
            keys,
            focus,
        }
    }

    pub fn mode(&self) -> BarMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: BarMode, cx: &mut Context<Self>) {
        self.mode = mode;
        cx.notify();
    }

    /// Offer to keep the take a restart replaced, until `until`; `None`
    /// withdraws the offer.
    pub fn offer_previous(&mut self, until: Option<Instant>, cx: &mut Context<Self>) {
        self.previous_until = until;
        cx.notify();
    }

    /// Show `text` in place of the time for `length`.
    pub fn notice(
        &mut self,
        text: impl Into<SharedString>,
        length: Duration,
        cx: &mut Context<Self>,
    ) {
        self.notice = Some((text.into(), Instant::now() + length));
        cx.notify();
    }

    /// The elapsed time as shown.
    pub fn time(&self) -> String {
        clock(self.clock.get().elapsed(Instant::now()))
    }

    fn ask(&mut self, event: RecordBarEvent, cx: &mut Context<Self>) {
        cx.emit(event);
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let request = match (self.mode, key) {
            (BarMode::Controls, "p" | "space") => Some(RecordBarEvent::TogglePause),
            (BarMode::Controls, "s") => Some(RecordBarEvent::Stop),
            (BarMode::Controls, "n") if !self.offering(Instant::now()) => {
                Some(RecordBarEvent::Restart)
            }
            (BarMode::Controls, "d") => Some(RecordBarEvent::Discard),
            (BarMode::Controls, "z") if self.offering(Instant::now()) => Some(RecordBarEvent::Undo),
            (BarMode::Confirm(..), "enter") => Some(RecordBarEvent::Confirm),
            (BarMode::Confirm(Destructive::Discard, _), "d") => Some(RecordBarEvent::Confirm),
            (BarMode::Confirm(Destructive::Restart, _), "n") => Some(RecordBarEvent::Confirm),
            (BarMode::Confirm(..), "escape") => Some(RecordBarEvent::Cancel),
            (BarMode::Discarded { .. }, "z") => Some(RecordBarEvent::Undo),
            (BarMode::Discarded { .. }, "enter" | "d") => Some(RecordBarEvent::Confirm),
            _ => None,
        };
        if let Some(request) = request {
            self.ask(request, cx);
        }
    }

    fn offering(&self, now: Instant) -> bool {
        self.previous_until.is_some_and(|until| until > now)
    }

    /// The key hint for a button: its letter while the bar has the
    /// keyboard, otherwise its global chord.
    fn hint(&self, letter: &'static str, chord: &str) -> SharedString {
        if self.active {
            letter.into()
        } else {
            chord.to_string().into()
        }
    }

    /// A button: an icon, a label and its key hint.
    #[expect(
        clippy::too_many_arguments,
        reason = "each is one part of the button, named at the call; a struct would only repeat the names"
    )]
    fn button(
        &self,
        id: &'static str,
        icon: IconName,
        label: &'static str,
        key: SharedString,
        color: Hsla,
        cx: &mut Context<Self>,
        event: RecordBarEvent,
    ) -> AnyElement {
        let key_id = SharedString::from(format!("{id}-key"));
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(label)
            .test_support()
            .flex()
            .flex_none()
            .items_center()
            .gap_1p5()
            .h(px(32.))
            .px_2()
            .rounded_md()
            .bg(tile())
            .hover(|s| s.bg(hover()))
            .cursor_pointer()
            .text_sm()
            .text_color(color)
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseUpEvent, _, cx| this.ask(event, cx)),
            )
            .child(Icon::new(icon).size(px(14.)))
            .child(label)
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

    /// The status text: a notice, or the time and what it means.
    fn status(&self, now: Instant) -> (SharedString, Hsla) {
        if let Some((text, until)) = &self.notice
            && *until > now
        {
            return (text.clone(), gpui_kit::white());
        }
        let is_paused = self.clock.get().is_paused();
        match self.mode {
            BarMode::Controls if is_paused => (self.time().into(), paused()),
            BarMode::Controls => (self.time().into(), gpui_kit::white()),
            BarMode::Confirm(Destructive::Discard, length) => (
                format!("Discard this {} recording?", clock(length)).into(),
                gpui_kit::white(),
            ),
            BarMode::Confirm(Destructive::Restart, length) => (
                format!("Restart? The {} take is thrown away.", clock(length)).into(),
                gpui_kit::white(),
            ),
            BarMode::Discarded { until } => (
                format!(
                    "Discarded · deleted in {} s",
                    until.saturating_duration_since(now).as_secs() + 1
                )
                .into(),
                muted(),
            ),
        }
    }

    fn buttons(&self, now: Instant, cx: &mut Context<Self>) -> Vec<AnyElement> {
        match self.mode {
            BarMode::Controls => self.control_buttons(now, cx),
            BarMode::Confirm(action, _) => self.confirm_buttons(action, cx),
            BarMode::Discarded { .. } => self.discarded_buttons(cx),
        }
    }

    /// While recording: Pause, Stop, Restart, and Discard set apart.
    fn control_buttons(&self, now: Instant, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let (white, keys) = (gpui_kit::white(), &self.keys);
        let (icon, label) = if self.clock.get().is_paused() {
            (IconName::Play, "Resume")
        } else {
            (IconName::Pause, "Pause")
        };
        // Just after a restart, keeping the previous take takes Restart's
        // place.
        let third = if self.offering(now) {
            self.button(
                "record-keep-previous",
                IconName::Undo,
                "Keep previous take",
                self.hint("Z", &keys.undo),
                white,
                cx,
                RecordBarEvent::Undo,
            )
        } else {
            self.button(
                "record-restart",
                IconName::RotateCcw,
                "Restart",
                self.hint("N", &keys.restart),
                white,
                cx,
                RecordBarEvent::Restart,
            )
        };
        vec![
            self.button(
                "record-pause",
                icon,
                label,
                self.hint("P", &keys.pause),
                white,
                cx,
                RecordBarEvent::TogglePause,
            ),
            self.button(
                "record-stop",
                IconName::Square,
                "Stop",
                self.hint("S", &keys.stop),
                white,
                cx,
                RecordBarEvent::Stop,
            ),
            third,
            // Set apart, at the end.
            div().w(px(4.)).into_any_element(),
            self.button(
                "record-discard",
                IconName::Trash,
                "Discard",
                self.hint("D", &keys.discard),
                recording(),
                cx,
                RecordBarEvent::Discard,
            ),
        ]
    }

    /// Asking before `action` throws the take away: keep recording, or go
    /// ahead.
    fn confirm_buttons(&self, action: Destructive, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let keys = &self.keys;
        let (icon, label, chord) = match action {
            Destructive::Discard => (IconName::Trash, "Discard", keys.discard.as_str()),
            Destructive::Restart => (IconName::RotateCcw, "Restart", keys.restart.as_str()),
        };
        vec![
            self.button(
                "record-keep",
                IconName::Play,
                "Keep recording",
                self.hint("Esc", &keys.pause),
                gpui_kit::white(),
                cx,
                RecordBarEvent::Cancel,
            ),
            self.button(
                "record-confirm",
                icon,
                label,
                self.hint("Enter", chord),
                recording(),
                cx,
                RecordBarEvent::Confirm,
            ),
        ]
    }

    /// After a discard, while it can still be undone.
    fn discarded_buttons(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let keys = &self.keys;
        vec![
            self.button(
                "record-undo",
                IconName::Undo,
                "Undo",
                self.hint("Z", &keys.undo),
                gpui_kit::white(),
                cx,
                RecordBarEvent::Undo,
            ),
            self.button(
                "record-discard-now",
                IconName::Trash,
                "Discard now",
                self.hint("Enter", &keys.discard),
                recording(),
                cx,
                RecordBarEvent::Confirm,
            ),
        ]
    }
}

impl Render for RecordBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let dot = match self.mode {
            BarMode::Discarded { .. } => muted(),
            _ if self.clock.get().is_paused() => paused(),
            _ => recording(),
        };
        let (status, color) = self.status(now);
        let buttons = self.buttons(now, cx);
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
            .border_color(border())
            .child(div().flex_none().size(px(10.)).rounded_full().bg(dot))
            .child(
                div()
                    .id("record-time")
                    .role(Role::Status)
                    .aria_label(status.clone())
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(color)
                    .child(status),
            )
            .children(buttons)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redraws_once_a_second_just_after_the_clock_ticks() {
        let now = Instant::now();
        let ms = Duration::from_millis;
        // 3.2 s in: the clock shows 0:04 in 0.8 s.
        let clock = Clock::new(now - ms(3_200));
        assert_eq!(next_redraw(clock, now), ms(805));
        // Paused, the time stands still; countdowns still move.
        let mut paused = clock;
        paused.pause(now);
        assert_eq!(next_redraw(paused, now), SECOND);
    }
}
