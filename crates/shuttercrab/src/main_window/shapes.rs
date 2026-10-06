//! The Shapes tool, as Snipping Tool's. G picks it up, and while it is in
//! hand a bar over the screenshot offers Emoji (E), Rectangle (R), Oval (O),
//! Line (L) and Arrow (A), then Fill (F) and Outline (T). Emoji opens its
//! 18, and the one picked lands in the middle of the view, picked up. Fill and Outline each open
//! their colours, Transparent first, and an opacity; Outline its size too.
//! Arrows and Enter pick a colour, - and + change the opacity, [ and ] the
//! size. Choices are kept in the settings.

use super::{MainWindow, tools::Hand};
use crate::{
    markup::{Ink, Rgb, SHAPE_COLORS, ShapeKind, ShapeStyle},
    palette::{border, coral, hover, muted, tile},
};
use gpui_kit::{
    AppContext as _, ClickEvent, Context, Entity, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Role, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, TestSupportExt as _, Window,
    assets::IconName,
    component::{
        Icon,
        slider::{Slider, SliderEvent, SliderState},
    },
    deferred, div,
    prelude::FluentBuilder as _,
    px, rgb,
};

/// Which of a shape's two colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Part {
    Fill,
    Outline,
}

/// Fill's or Outline's menu, open below its button.
pub(super) struct ShapeMenu {
    part: Part,
    /// The colour the arrow keys are on.
    highlighted: usize,
    opacity: Entity<SliderState>,
    /// Outline's only.
    size: Option<Entity<SliderState>>,
    _changes: Vec<Subscription>,
}

/// Swatches in a row, as in Snipping Tool.
const COLUMNS: usize = 6;

/// How far - and + move the opacity, percent.
const OPACITY_STEP: u8 = 10;

impl Part {
    fn ink(self, style: &ShapeStyle) -> Ink {
        match self {
            Part::Fill => style.fill,
            Part::Outline => style.outline,
        }
    }

    fn ink_mut(self, style: &mut ShapeStyle) -> &mut Ink {
        match self {
            Part::Fill => &mut style.fill,
            Part::Outline => &mut style.outline,
        }
    }
}

impl ShapeKind {
    fn name(self) -> &'static str {
        match self {
            ShapeKind::Rectangle => "Rectangle",
            ShapeKind::Oval => "Oval",
            ShapeKind::Line => "Line",
            ShapeKind::Arrow => "Arrow",
            ShapeKind::Emoji(_) => "Emoji",
        }
    }

    fn key(self) -> &'static str {
        match self {
            ShapeKind::Rectangle => "r",
            ShapeKind::Oval => "o",
            ShapeKind::Line => "l",
            ShapeKind::Arrow => "a",
            ShapeKind::Emoji(_) => "e",
        }
    }

    fn icon(self) -> IconName {
        match self {
            ShapeKind::Rectangle => IconName::Square,
            ShapeKind::Oval => IconName::Circle,
            ShapeKind::Line => IconName::Slash,
            ShapeKind::Arrow => IconName::MoveUpLeft,
            ShapeKind::Emoji(_) => IconName::FaceSlightlySmiling,
        }
    }
}

impl MainWindow {
    /// What the next shape is drawn with.
    pub(super) fn shape_style(&self) -> ShapeStyle {
        self.settings().shape_style()
    }

    /// Choose `style` for the next shape; a picked-up shape takes on its
    /// changed colours and size too.
    fn set_shape_style(&mut self, style: ShapeStyle, cx: &mut Context<Self>) {
        let before = self.shape_style();
        self.change(cx, |settings| settings.set_shape_style(style));
        self.restyle_selection(before, style, cx);
    }

    /// Whether the Shapes bar shows: while the shapes are in hand, or
    /// Select with a shape picked up.
    pub(super) fn shapes_bar_shown(&self) -> bool {
        let selected = self.shown.as_ref().is_some_and(|s| s.selected.is_some());
        match self.hand {
            Some(Hand::Shape) => true,
            Some(Hand::Select) => selected,
            _ => false,
        }
    }

    /// Whether Fill or Outline can be chosen: for the shape picked up, or
    /// the next one. An emoji has neither; a line or an arrow, no fill.
    fn ink_enabled(&self, part: Part, style: &ShapeStyle) -> bool {
        let picked_up = self
            .shown
            .as_ref()
            .and_then(|shown| shown.selected_shape())
            .map(|(_, shape)| shape.kind);
        match picked_up.unwrap_or(style.kind) {
            ShapeKind::Emoji(_) => false,
            kind => part == Part::Outline || kind.fills(),
        }
    }

    /// Draw `kind` next. Fill's menu closes for a shape without a fill.
    fn pick_shape(&mut self, kind: ShapeKind, cx: &mut Context<Self>) {
        let style = ShapeStyle {
            kind,
            ..self.shape_style()
        };
        self.set_shape_style(style, cx);
        if !kind.fills()
            && self
                .shape_menu
                .as_ref()
                .is_some_and(|m| m.part == Part::Fill)
        {
            self.shape_menu = None;
        }
        cx.notify();
    }

    /// Open `part`'s menu, or close it if open. A line or an arrow has no
    /// fill to choose.
    fn toggle_shape_menu(&mut self, part: Part, cx: &mut Context<Self>) {
        let style = self.shape_style();
        self.emoji_menu = None;
        let was = self.shape_menu.take().map(|menu| menu.part);
        if was != Some(part) && self.ink_enabled(part, &style) {
            self.open_shape_menu(part, style, cx);
        }
        cx.notify();
    }

    fn open_shape_menu(&mut self, part: Part, style: ShapeStyle, cx: &mut Context<Self>) {
        let ink = part.ink(&style);
        let opacities = ShapeStyle::OPACITIES;
        let opacity = cx.new(|_| {
            SliderState::new()
                .min(f32::from(*opacities.start()))
                .max(f32::from(*opacities.end()))
                .step(1.)
                .default_value(f32::from(ink.opacity))
        });
        let mut changes = vec![
            cx.subscribe(&opacity, move |this, _, event: &SliderEvent, cx| {
                if let SliderEvent::Change(value) = event {
                    let mut style = this.shape_style();
                    part.ink_mut(&mut style).opacity = value.end().round() as u8;
                    this.set_shape_style(style, cx);
                }
            }),
        ];
        let size = (part == Part::Outline).then(|| {
            let sizes = ShapeStyle::SIZES;
            let size = cx.new(|_| {
                SliderState::new()
                    .min(*sizes.start())
                    .max(*sizes.end())
                    .step(1.)
                    .default_value(style.size)
            });
            changes.push(cx.subscribe(&size, |this, _, event: &SliderEvent, cx| {
                if let SliderEvent::Change(value) = event {
                    let style = ShapeStyle {
                        size: value.end().round(),
                        ..this.shape_style()
                    };
                    this.set_shape_style(style, cx);
                }
            }));
            size
        });
        let highlighted = SHAPE_COLORS
            .iter()
            .position(|c| *c == ink.color)
            .unwrap_or(0);
        self.shape_menu = Some(ShapeMenu {
            part,
            highlighted,
            opacity,
            size,
            _changes: changes,
        });
    }

    fn pick_ink(&mut self, part: Part, color: Option<Rgb>, cx: &mut Context<Self>) {
        let mut style = self.shape_style();
        part.ink_mut(&mut style).color = color;
        self.set_shape_style(style, cx);
    }

    /// Minus and plus: the open menu's opacity, a step lower or higher.
    /// Nothing for Transparent, which has none to change.
    fn step_opacity(&mut self, steps: i16, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = &self.shape_menu else {
            return;
        };
        let (part, slider) = (menu.part, menu.opacity.clone());
        let mut style = self.shape_style();
        let ink = part.ink_mut(&mut style);
        if ink.color.is_none() {
            return;
        }
        let opacities = ShapeStyle::OPACITIES;
        let opacity = (i16::from(ink.opacity) + steps * i16::from(OPACITY_STEP))
            .clamp(i16::from(*opacities.start()), i16::from(*opacities.end()))
            as u8;
        ink.opacity = opacity;
        self.set_shape_style(style, cx);
        slider.update(cx, |slider, cx| {
            slider.set_value(f32::from(opacity), window, cx)
        });
    }

    /// [ and ] with the Shapes tool: the outline a size smaller or bigger.
    pub(super) fn step_shape_size(
        &mut self,
        steps: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let style = self.shape_style();
        let sizes = ShapeStyle::SIZES;
        let size = (style.size + steps).clamp(*sizes.start(), *sizes.end());
        self.set_shape_style(ShapeStyle { size, ..style }, cx);
        self.note_size(cx);
        if let Some(slider) = self.shape_menu.as_ref().and_then(|m| m.size.clone()) {
            slider.update(cx, |slider, cx| slider.set_value(size, window, cx));
        }
    }

    /// A key while the Shapes tool is in hand: a shape's key picks it, F
    /// and T open Fill and Outline; in their menu, arrows move through the
    /// colours, Enter or Space picks one and closes it, - and + change the
    /// opacity. Returns whether the key was used.
    pub(super) fn on_shapes_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.shapes_bar_shown() {
            return false;
        }
        if let Some(kind) = ShapeKind::ALL.into_iter().find(|k| k.key() == key) {
            self.pick_shape(kind, cx);
            return true;
        }
        if self.on_emoji_key(key, window, cx) {
            return true;
        }
        match key {
            "f" => return self.toggle_shape_menu_by_key(Part::Fill, cx),
            "t" => return self.toggle_shape_menu_by_key(Part::Outline, cx),
            "-" => {
                self.step_opacity(-1, window, cx);
                return self.shape_menu.is_some();
            }
            "=" | "+" => {
                self.step_opacity(1, window, cx);
                return self.shape_menu.is_some();
            }
            _ => {}
        }
        let Some(menu) = &mut self.shape_menu else {
            return false;
        };
        let count = SHAPE_COLORS.len();
        let at = menu.highlighted;
        menu.highlighted = match key {
            "left" => (at + count - 1) % count,
            "right" => (at + 1) % count,
            "up" => (at + count - COLUMNS) % count,
            "down" => (at + COLUMNS) % count,
            "enter" | "space" => {
                let part = menu.part;
                self.shape_menu = None;
                self.pick_ink(part, SHAPE_COLORS[at], cx);
                return true;
            }
            _ => return false,
        };
        cx.notify();
        true
    }

    fn toggle_shape_menu_by_key(&mut self, part: Part, cx: &mut Context<Self>) -> bool {
        self.toggle_shape_menu(part, cx);
        true
    }

    /// The Shapes bar, over the top of the screenshot while the Shapes
    /// tool is in hand.
    pub(super) fn shapes_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if !self.shapes_bar_shown() {
            return None;
        }
        let style = self.shape_style();
        let separator = || div().w(px(1.)).h(px(28.)).mx_1().bg(border());
        let shapes = ShapeKind::ALL.map(|kind| Self::shape_button(kind, kind == style.kind, cx));
        Some(
            div()
                .absolute()
                .top(px(6.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("shapes-bar")
                        .role(Role::Toolbar)
                        .test_support()
                        .flex()
                        .items_center()
                        .gap_1()
                        .p(px(4.))
                        .rounded_lg()
                        .bg(rgb(0x2C2C2C))
                        .border_1()
                        .border_color(border())
                        .shadow_lg()
                        // A press here is not a shape, nor a press elsewhere.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(self.emoji_button(cx))
                        .child(separator())
                        .children(shapes)
                        .child(separator())
                        .child(self.ink_button(Part::Fill, &style, cx))
                        .child(self.ink_button(Part::Outline, &style, cx)),
                ),
        )
    }

    fn shape_button(
        kind: ShapeKind,
        chosen: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let key = kind.key().to_uppercase();
        div()
            .id(SharedString::from(format!("shape-{}", kind.key())))
            .role(Role::Button)
            .aria_label(SharedString::from(format!("{} ({key})", kind.name())))
            .test_support()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .size(px(40.))
            .rounded_md()
            .border_1()
            .border_color(if chosen {
                coral()
            } else {
                gpui_kit::transparent_black()
            })
            .when(chosen, |d| d.bg(tile()))
            .hover(|s| s.bg(hover()))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.pick_shape(kind, cx)))
            .child(corner_key(key))
            .child(Icon::new(kind.icon()).size(px(18.)))
    }

    /// Fill or Outline: its colour, its name and its key, with its menu
    /// below when open. Fill is dimmed for a line or an arrow.
    fn ink_button(
        &self,
        part: Part,
        style: &ShapeStyle,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let (id, name, key) = match part {
            Part::Fill => ("shape-fill", "Fill", "F"),
            Part::Outline => ("shape-outline", "Outline", "T"),
        };
        let enabled = self.ink_enabled(part, style);
        let ink = part.ink(style);
        let swatch = match ink.color {
            None => Icon::new(IconName::Ban)
                .size(px(16.))
                .text_color(muted())
                .into_any_element(),
            Some(color) => {
                let circle = div().size(px(16.)).rounded_full();
                match part {
                    Part::Fill => circle.bg(rgb(color.hex())),
                    Part::Outline => circle.border(px(4.)).border_color(rgb(color.hex())),
                }
                .into_any_element()
            }
        };
        let menu = self
            .shape_menu
            .as_ref()
            .filter(|menu| menu.part == part && enabled);
        div()
            .relative()
            .child(
                div()
                    .id(id)
                    .role(Role::Button)
                    .aria_label(SharedString::from(format!("{name} ({key})")))
                    .test_support()
                    .relative()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .h(px(40.))
                    .pl_2p5()
                    .pr_4()
                    .rounded_md()
                    .when(menu.is_some(), |d| d.bg(tile()))
                    .when(!enabled, |d| d.opacity(0.35))
                    .when(enabled, |d| {
                        d.hover(|s| s.bg(hover()))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.toggle_shape_menu(part, cx)
                            }))
                    })
                    .child(corner_key(key.into()))
                    .child(swatch)
                    .child(div().text_sm().child(name))
                    .child(Icon::new(IconName::ChevronDown).size(px(14.))),
            )
            .when_some(menu, |d, menu| {
                d.child(deferred(self.shape_menu_panel(menu, style, cx)).with_priority(1))
            })
    }

    /// Fill's or Outline's menu: "Colours", Transparent first, the chosen
    /// one ringed; "Opacity", dimmed for Transparent; Outline's "Size" with
    /// a preview line.
    fn shape_menu_panel(
        &self,
        menu: &ShapeMenu,
        style: &ShapeStyle,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let part = menu.part;
        let ink = part.ink(style);
        let swatches = SHAPE_COLORS.iter().enumerate().map(|(i, &color)| {
            let chosen = color == ink.color;
            let highlighted = i == menu.highlighted;
            let label = color.map_or("Transparent".to_string(), Rgb::to_hex_string);
            div()
                .id(SharedString::from(format!("shape-color-{i}")))
                .role(Role::Button)
                .aria_label(SharedString::from(label))
                .test_support()
                .size(px(36.))
                .rounded_full()
                .p(px(3.))
                .border_2()
                .border_color(if chosen {
                    gpui_kit::white()
                } else if highlighted {
                    muted()
                } else {
                    gpui_kit::transparent_black()
                })
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(menu) = &mut this.shape_menu {
                        menu.highlighted = i;
                    }
                    this.pick_ink(part, color, cx);
                }))
                .child(match color {
                    Some(color) => div()
                        .size_full()
                        .rounded_full()
                        .bg(rgb(color.hex()))
                        .into_any_element(),
                    None => div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(IconName::Ban).size(px(26.)))
                        .into_any_element(),
                })
        });
        let hint = match part {
            Part::Fill => "Arrows and Enter pick a colour; - and + change the opacity",
            Part::Outline => {
                "Arrows and Enter pick a colour; - and + change the opacity; [ and ] the size"
            }
        };
        let preview = style.outline.color.map_or(muted(), |c| rgb(c.hex()).into());
        div()
            .id("shape-menu")
            .test_support()
            .absolute()
            .top(px(46.))
            .left(px(0.))
            .w(px(COLUMNS as f32 * 46. + 30.))
            .p(px(15.))
            .flex()
            .flex_col()
            .gap_3()
            .rounded_lg()
            .bg(rgb(0x2C2C2C))
            .border_1()
            .border_color(border())
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().text_sm().child("Colours"))
            .child(div().flex().flex_wrap().gap(px(10.)).children(swatches))
            .child(
                div()
                    .text_sm()
                    .when(ink.color.is_none(), |d| d.text_color(muted()))
                    .child("Opacity"),
            )
            .child(
                Slider::new(&menu.opacity)
                    .horizontal()
                    .disabled(ink.color.is_none()),
            )
            .when_some(menu.size.as_ref(), |d, size| {
                d.child(div().text_sm().child("Size"))
                    .child(super::tools::preview(preview, style.size))
                    .child(Slider::new(size).horizontal())
            })
            .child(div().text_xs().text_color(muted()).child(hint))
    }
}

/// A key letter, small in a button's top right corner.
fn corner_key(key: String) -> impl IntoElement {
    div()
        .absolute()
        .top(px(1.))
        .right(px(3.))
        .text_size(px(9.))
        .text_color(muted())
        .child(key)
}
