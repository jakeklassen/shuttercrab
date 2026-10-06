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
//!
//! Before recording, as in the Snipping Tool, the bar is ready rather than
//! recording: it has the keyboard, and offers Start (Enter), Cancel (Esc),
//! and the sound to record, starting from the settings each time: the
//! microphone (M), which one (Down opens the list), and the system sound
//! (A). Choices there are for that recording only. Both switches stay
//! while recording, so either source can be switched on or off mid-take.

use crate::{
    palette::{border, hover, muted, paused, recording, surface, tile},
    recording::{Clock, clock},
};
use gpui_kit::{
    AnyElement, Bounds, ClickEvent, Context, EventEmitter, FocusHandle, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Pixels, Render, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
    assets::IconName, canvas, component::Icon, div, prelude::FluentBuilder as _, px,
};
use shuttercrab_capture::record::{Microphone, Source};
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
    /// Start recording, from the ready bar.
    Start,
    /// Close the ready bar without recording.
    Close,
    /// Switch a sound source on or off; the bar already shows it.
    SetSound(Source, bool),
    /// Open the list of microphones.
    ChooseMicrophone,
}

/// The sound a recording takes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sound {
    /// What the speakers play.
    pub system: bool,
    pub microphone: bool,
    /// Which microphone: Windows' id for it, or `None` for Windows'
    /// default.
    pub device: Option<String>,
}

impl Sound {
    pub fn is_on(&self, source: Source) -> bool {
        match source {
            Source::System => self.system,
            Source::Microphone => self.microphone,
        }
    }

    fn set(&mut self, source: Source, on: bool) {
        match source {
            Source::System => self.system = on,
            Source::Microphone => self.microphone = on,
        }
    }
}

/// The microphone list button's width, logical pixels: long names are cut
/// short.
const MICROPHONE_LIST_WIDTH: f32 = 220.0;

/// How the microphone list names Windows' default.
pub const DEFAULT_MICROPHONE: &str = "Windows' default";

/// An action that throws a take away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destructive {
    Discard,
    Restart,
}

/// What the bar shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarMode {
    /// Before recording: Start, Cancel and the sound to record.
    Ready,
    /// Start was pressed; the countdown runs or the recorder starts.
    Starting,
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
pub const RECORD_BAR_WIDTH: f32 = 920.0;
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
    /// The sound to record, as switched in the bar.
    sound: Sound,
    /// The microphones plugged in when the bar opened, Windows' default
    /// first.
    microphones: Vec<Microphone>,
    /// Where the microphone list's button was last drawn, so the list can
    /// open below it.
    list_button: Rc<Cell<Bounds<Pixels>>>,
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
            sound: Sound::default(),
            microphones: Vec::new(),
            list_button: Rc::default(),
            focus,
        }
    }

    /// Show the ready bar, recording `sound` unless switched, with
    /// `microphones` to choose from. A chosen microphone that is not
    /// plugged in gives way to Windows' default.
    pub fn ready(mut self, mut sound: Sound, microphones: Vec<Microphone>) -> Self {
        if let Some(id) = &sound.device
            && !microphones.iter().any(|m| &m.id == id)
        {
            log::info!("the chosen microphone is not plugged in; Windows' default is used");
            sound.device = None;
        }
        self.mode = BarMode::Ready;
        self.sound = sound;
        self.microphones = microphones;
        self
    }

    pub fn mode(&self) -> BarMode {
        self.mode
    }

    pub fn sound(&self) -> &Sound {
        &self.sound
    }

    /// The microphone list's choices: Windows' default, then each one
    /// plugged in. The second value is the index of the one chosen.
    pub fn microphone_choices(&self) -> (Vec<SharedString>, usize) {
        let names = std::iter::once(DEFAULT_MICROPHONE.into())
            .chain(self.microphones.iter().map(|m| m.name.clone().into()))
            .collect();
        let chosen = self
            .sound
            .device
            .as_ref()
            .and_then(|id| self.microphones.iter().position(|m| &m.id == id))
            .map_or(0, |i| i + 1);
        (names, chosen)
    }

    /// Use the microphone list's choice `index`.
    pub fn choose_microphone(&mut self, index: usize, cx: &mut Context<Self>) {
        self.sound.device = index
            .checked_sub(1)
            .and_then(|i| self.microphones.get(i))
            .map(|m| m.id.clone());
        cx.notify();
    }

    /// Where the microphone list's button is, logical pixels in the bar.
    pub fn list_button(&self) -> Bounds<Pixels> {
        self.list_button.get()
    }

    /// The chosen microphone's name.
    fn microphone_name(&self) -> SharedString {
        let (names, chosen) = self.microphone_choices();
        names[chosen].clone()
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
        match event {
            RecordBarEvent::SetSound(source, on) => self.sound.set(source, on),
            RecordBarEvent::Start => self.mode = BarMode::Starting,
            _ => {}
        }
        cx.emit(event);
        cx.notify();
    }

    /// Switch `source` the other way.
    fn flip(&self, source: Source) -> RecordBarEvent {
        RecordBarEvent::SetSound(source, !self.sound.is_on(source))
    }

    /// Whether the sound switches are shown.
    fn switches_shown(&self) -> bool {
        matches!(
            self.mode,
            BarMode::Ready | BarMode::Starting | BarMode::Controls
        )
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let request = match (self.mode, key) {
            (_, "m") if self.switches_shown() => Some(self.flip(Source::Microphone)),
            (_, "a") if self.switches_shown() => Some(self.flip(Source::System)),
            (BarMode::Ready, "enter") => Some(RecordBarEvent::Start),
            (BarMode::Ready, "escape") => Some(RecordBarEvent::Close),
            (BarMode::Ready, "down") => Some(RecordBarEvent::ChooseMicrophone),
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

    /// The key hint for a button with no global chord: its key while the
    /// bar has the keyboard, otherwise none. The ready bar always shows
    /// them: it gets the keyboard back from the microphone list, and hints
    /// coming and going would move its buttons.
    fn letter(&self, letter: &'static str) -> SharedString {
        if self.active || self.mode == BarMode::Ready {
            letter.into()
        } else {
            "".into()
        }
    }

    /// A button's key hint, if it has one.
    fn key_hint(id: &'static str, key: SharedString) -> Option<AnyElement> {
        (!key.is_empty()).then(|| {
            div()
                .id(SharedString::from(format!("{id}-key")))
                .role(Role::Status)
                .aria_label(key.clone())
                .test_support()
                .text_xs()
                .text_color(muted())
                .child(key)
                .into_any_element()
        })
    }

    /// A switch for a sound source, its icon crossed out when off. In the
    /// ready bar it is named; while recording, the icon alone.
    fn sound_switch(&self, source: Source, cx: &mut Context<Self>) -> AnyElement {
        let on = self.sound.is_on(source);
        let (id, name, icon, letter) = match source {
            Source::Microphone => (
                "record-microphone",
                "Microphone",
                if on { IconName::Mic } else { IconName::MicOff },
                "M",
            ),
            Source::System => (
                "record-system-sound",
                "System sound",
                if on {
                    IconName::Volume2
                } else {
                    IconName::VolumeX
                },
                "A",
            ),
        };
        let label = format!("{name} {}", if on { "on" } else { "off" });
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
            .text_color(if on { gpui_kit::white() } else { muted() })
            .on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| this.ask(this.flip(source), cx)),
            )
            .child(Icon::new(icon).size(px(14.)))
            .when(self.mode == BarMode::Ready, |d| d.child(name))
            .children(Self::key_hint(id, self.letter(letter)))
            .into_any_element()
    }

    /// The button that opens the microphone list, naming the one chosen.
    fn microphone_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let name = self.microphone_name();
        let drawn = self.list_button.clone();
        div()
            .id("record-microphones")
            .role(Role::Button)
            .aria_label(format!("Which microphone: {name}"))
            .test_support()
            .relative()
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
            .text_color(gpui_kit::white())
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.ask(RecordBarEvent::ChooseMicrophone, cx)
            }))
            // As wide whichever is chosen, so nothing moves when it changes.
            .w(px(MICROPHONE_LIST_WIDTH))
            .child(div().flex_1().min_w_0().truncate().child(name))
            .child(Icon::new(IconName::ChevronDown).size(px(14.)))
            .children(Self::key_hint("record-microphones", self.letter("↓")))
            .child(
                canvas(move |bounds, _, _| drawn.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
            .into_any_element()
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
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.ask(event, cx)))
            .child(Icon::new(icon).size(px(14.)))
            .child(label)
            .children(Self::key_hint(id, key))
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
            BarMode::Ready => ("Ready to record".into(), gpui_kit::white()),
            BarMode::Starting => ("Starting…".into(), muted()),
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
            BarMode::Ready => self.ready_buttons(cx),
            BarMode::Starting => self.switches(cx),
            BarMode::Controls => {
                let mut buttons = self.switches(cx);
                buttons.extend(self.control_buttons(now, cx));
                buttons
            }
            BarMode::Confirm(action, _) => self.confirm_buttons(action, cx),
            BarMode::Discarded { .. } => self.discarded_buttons(cx),
        }
    }

    /// Before recording: the sound to record, then Start and Cancel.
    fn ready_buttons(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            self.sound_switch(Source::Microphone, cx),
            self.microphone_list(cx),
            self.sound_switch(Source::System, cx),
            div().w(px(4.)).into_any_element(),
            self.button(
                "record-start",
                IconName::Circle,
                "Start",
                self.letter("Enter"),
                recording(),
                cx,
                RecordBarEvent::Start,
            ),
            self.button(
                "record-close",
                IconName::X,
                "Cancel",
                self.letter("Esc"),
                gpui_kit::white(),
                cx,
                RecordBarEvent::Close,
            ),
        ]
    }

    /// The sound switches alone, set apart from what follows.
    fn switches(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            self.sound_switch(Source::Microphone, cx),
            self.sound_switch(Source::System, cx),
            div().w(px(4.)).into_any_element(),
        ]
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
            BarMode::Ready | BarMode::Starting | BarMode::Discarded { .. } => muted(),
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
