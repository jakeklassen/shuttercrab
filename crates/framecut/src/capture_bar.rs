//! The Capture Bar (PRD §7.5): a small bar at the top of the screen that
//! asks whether to take a screenshot or record, and of what. It opens from
//! its own hotkey or the tray icon, remembers the last choice, works from
//! the keyboard, and disappears as soon as a target is chosen.

use gpui_kit::{
    Context, EventEmitter, FocusHandle, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent,
    MouseButton, MouseUpEvent, ParentElement as _, Render, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, assets::IconName,
    component::Icon, div, prelude::FluentBuilder as _, px, rgb,
};
use serde::{Deserialize, Serialize};

/// Whether the bar takes a screenshot or starts a recording.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureMode {
    #[default]
    Screenshot,
    Record,
}

impl CaptureMode {
    const ALL: [CaptureMode; 2] = [Self::Screenshot, Self::Record];

    fn label(self) -> &'static str {
        match self {
            Self::Screenshot => "Screenshot",
            Self::Record => "Record",
        }
    }

    /// The letter that switches to it from the keyboard.
    fn key(self) -> &'static str {
        match self {
            Self::Screenshot => "s",
            Self::Record => "r",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Screenshot => IconName::Camera,
            Self::Record => IconName::Video,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Screenshot => "mode-screenshot",
            Self::Record => "mode-record",
        }
    }

    /// Whether `target` can be captured this way: recordings are of an
    /// area or a display (PRD §7.7).
    pub fn offers(self, target: CaptureTarget) -> bool {
        self == Self::Screenshot || target != CaptureTarget::Window
    }
}

/// What a screenshot or recording captures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureTarget {
    #[default]
    Area,
    Window,
    Display,
}

impl CaptureTarget {
    pub const ALL: [CaptureTarget; 3] = [Self::Area, Self::Window, Self::Display];

    fn label(self) -> &'static str {
        match self {
            Self::Area => "Area",
            Self::Window => "Window",
            Self::Display => "Display",
        }
    }

    /// The letter that picks it from the keyboard.
    fn key(self) -> &'static str {
        match self {
            Self::Area => "a",
            Self::Window => "w",
            Self::Display => "d",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Area => IconName::SquareDashed,
            Self::Window => IconName::AppWindow,
            Self::Display => IconName::Monitor,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Area => "target-area",
            Self::Window => "target-window",
            Self::Display => "target-display",
        }
    }
}

/// What the user did with the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureBarEvent {
    Chosen(CaptureMode, CaptureTarget),
    Dismissed,
}

/// The bar's size in logical pixels.
pub const BAR_WIDTH: f32 = 312.0;
pub const BAR_HEIGHT: f32 = 132.0;

pub struct CaptureBar {
    mode: CaptureMode,
    selected: CaptureTarget,
    focus: FocusHandle,
    done: bool,
}

impl EventEmitter<CaptureBarEvent> for CaptureBar {}

fn surface() -> Hsla {
    rgb(0x202020).into()
}

fn tile() -> Hsla {
    rgb(0x2B2B2B).into()
}

fn muted() -> Hsla {
    rgb(0x9D9D9D).into()
}

fn accent() -> Hsla {
    rgb(0x1F6FEB).into()
}

impl CaptureBar {
    /// A bar with `selected` highlighted: the last target used.
    pub fn new(selected: CaptureTarget, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // Clicking anywhere else dismisses the bar.
        cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                this.finish(CaptureBarEvent::Dismissed, cx);
            }
        })
        .detach();
        Self {
            mode: CaptureMode::Screenshot,
            selected,
            focus,
            done: false,
        }
    }

    /// Start in `mode` rather than Screenshot.
    pub fn with_mode(mut self, mode: CaptureMode) -> Self {
        self.set_mode(mode);
        self
    }

    pub fn mode(&self) -> CaptureMode {
        self.mode
    }

    pub fn selected(&self) -> CaptureTarget {
        self.selected
    }

    /// Switch modes, moving off a target the new mode does not offer.
    fn set_mode(&mut self, mode: CaptureMode) {
        self.mode = mode;
        if !mode.offers(self.selected) {
            self.selected = CaptureTarget::Area;
        }
    }

    fn choose(&mut self, target: CaptureTarget, cx: &mut Context<Self>) {
        if self.mode.offers(target) {
            self.selected = target;
            self.finish(CaptureBarEvent::Chosen(self.mode, target), cx);
        }
    }

    /// Report once; later input and deactivation are ignored.
    fn finish(&mut self, event: CaptureBarEvent, cx: &mut Context<Self>) {
        if !std::mem::replace(&mut self.done, true) {
            cx.emit(event);
        }
    }

    /// Move the selection `by` targets, wrapping, over the ones this mode
    /// offers.
    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        let offered: Vec<_> = CaptureTarget::ALL
            .into_iter()
            .filter(|t| self.mode.offers(*t))
            .collect();
        let at = offered
            .iter()
            .position(|t| *t == self.selected)
            .unwrap_or(0) as isize;
        self.selected = offered[(at + by).rem_euclid(offered.len() as isize) as usize];
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match key {
            "escape" => self.finish(CaptureBarEvent::Dismissed, cx),
            "enter" | "space" => self.choose(self.selected, cx),
            "left" | "up" => self.step(-1, cx),
            "right" | "down" => self.step(1, cx),
            "tab" if event.keystroke.modifiers.shift => self.step(-1, cx),
            "tab" => self.step(1, cx),
            _ => {
                if let Some(mode) = CaptureMode::ALL.into_iter().find(|m| m.key() == key) {
                    self.set_mode(mode);
                    cx.notify();
                } else if let Some(target) = CaptureTarget::ALL.into_iter().find(|t| t.key() == key)
                {
                    self.choose(target, cx);
                }
            }
        }
    }

    fn mode_tab(&self, mode: CaptureMode, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let on = mode == self.mode;
        div()
            .id(mode.id())
            .role(Role::Tab)
            .aria_label(mode.label())
            .test_support()
            .flex()
            .items_center()
            .gap_1p5()
            .px_2()
            .py_1()
            .rounded_md()
            .when_else(
                on,
                |d| d.bg(tile()).text_color(gpui_kit::white()),
                |d| {
                    d.text_color(muted())
                        .hover(|s| s.text_color(gpui_kit::white()))
                },
            )
            .cursor_pointer()
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                    this.set_mode(mode);
                    cx.notify();
                }),
            )
            .text_sm()
            .child(
                Icon::new(mode.icon())
                    .size(px(16.))
                    .when(on && mode == CaptureMode::Record, |icon| {
                        icon.text_color(recording())
                    }),
            )
            .child(mode.label())
            .child(
                div()
                    .text_xs()
                    .text_color(muted())
                    .child(mode.key().to_uppercase()),
            )
    }

    fn target(&self, target: CaptureTarget, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let offered = self.mode.offers(target);
        let selected = offered && target == self.selected;
        let label: SharedString = target.label().into();
        let text = if offered { gpui_kit::white() } else { muted() };
        div()
            .id(target.id())
            .role(Role::Button)
            .aria_label(label.clone())
            .test_support()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .h(px(72.))
            .rounded_md()
            .border_2()
            .border_color(if selected { accent() } else { tile() })
            .bg(tile())
            .when(!offered, |d| d.opacity(0.4))
            .when(offered, |d| {
                d.hover(|s| s.bg(rgb(0x353535)))
                    .cursor_pointer()
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseUpEvent, _, cx| this.choose(target, cx)),
                    )
            })
            .child(
                Icon::new(target.icon())
                    .size(px(22.))
                    .text_color(if selected { accent() } else { text }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_sm()
                    .text_color(text)
                    .child(label)
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted())
                            .child(target.key().to_uppercase()),
                    ),
            )
    }
}

/// The red of a recording in progress.
fn recording() -> Hsla {
    rgb(0xE5484D).into()
}

impl Render for CaptureBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("capture-bar")
            .role(Role::Dialog)
            .aria_label("Capture Bar")
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
            .border_color(rgb(0x3A3A3A))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .children(CaptureMode::ALL.map(|m| self.mode_tab(m, cx))),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .children(CaptureTarget::ALL.map(|t| self.target(t, cx))),
            )
    }
}
