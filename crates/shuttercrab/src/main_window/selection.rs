//! Changing a shape after it is drawn. In Snipping Tool, the shape just
//! drawn can be changed until you click away; here any shape can be picked
//! up again too, with Select (V).
//!
//! The selected shape shows its box with a handle at each corner (a line
//! or an arrow, at each end) and, for a rectangle or an oval, a rotate
//! button above it. Drag the shape to move it, a handle to resize it, the
//! rotate button to turn it (Shift: in steps of 15°). Arrow keys move it a
//! pixel, Shift+arrows resize it, Alt+Left and Alt+Right turn it 15°,
//! Delete takes it off, Escape lets it go. Right-click offers Delete and
//! quarter and half turns. Fill, Outline and size apply to it.

use super::{
    MainWindow,
    canvas::{Gesture, Shown},
    tools::Hand,
};
use crate::{
    markup::{Edit, Mark, Shape, ShapeKind, ShapeStyle},
    palette::border,
    shot_view::Xy,
};
use gpui_kit::{
    AnyElement, Bounds, ClickEvent, Context, InteractiveElement as _, IntoElement, Keystroke,
    MouseButton, ParentElement as _, PathBuilder, Pixels, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, assets::IconName,
    canvas, component::Icon, deferred, div, point, prelude::FluentBuilder as _, px, rgb,
};

/// Which part of the selected shape a press took hold of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Grip {
    /// The shape itself: dragging moves it.
    Body,
    /// A handle, as [`Shape::handles`] lists them: dragging resizes.
    Handle(usize),
    /// The rotate button: dragging turns it about its centre.
    Turn,
}

/// The selected shape being dragged.
pub(super) struct Editing {
    pub(super) index: usize,
    original: Shape,
    pub(super) edited: Shape,
    pub(super) grip: Grip,
    /// Where the press was, screenshot pixels and canvas.
    from: (f32, f32),
    from_at: Xy,
    /// Whether the drag has gone far enough to change anything.
    moved: bool,
}

/// The right-click menu on a shape, where it opened in the canvas, and the
/// item the arrow keys are on.
pub(super) struct ShapeContext {
    at: Xy,
    highlighted: usize,
}

/// The right-click menu's items, as Snipping Tool's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContextItem {
    Delete,
    TurnRight,
    TurnLeft,
    TurnHalf,
}

impl ContextItem {
    const ALL: [ContextItem; 4] = [
        ContextItem::Delete,
        ContextItem::TurnRight,
        ContextItem::TurnLeft,
        ContextItem::TurnHalf,
    ];

    fn label(self) -> &'static str {
        match self {
            ContextItem::Delete => "Delete",
            ContextItem::TurnRight => "Rotate right 90°",
            ContextItem::TurnLeft => "Rotate left 90°",
            ContextItem::TurnHalf => "Rotate 180°",
        }
    }

    fn id(self) -> &'static str {
        match self {
            ContextItem::Delete => "context-delete",
            ContextItem::TurnRight => "context-turn-right",
            ContextItem::TurnLeft => "context-turn-left",
            ContextItem::TurnHalf => "context-turn-half",
        }
    }
}

/// How near a handle a press takes it, logical pixels on screen.
const HANDLE_REACH: f32 = 12.;

/// How near the shape a press takes it, logical pixels on screen.
const BODY_REACH: f32 = 6.;

/// A handle's size, logical pixels.
const HANDLE_SIZE: f32 = 10.;

/// The rotate button's size, and how far above the box its centre sits,
/// logical pixels.
const TURN_SIZE: f32 = 28.;
const TURN_OFFSET: f32 = 30.;

/// What Shift+arrows add to a side, and Alt+arrows and Shift on the rotate
/// button turn by: Snipping Tool's.
const GROW_STEP: f32 = 10.;
const TURN_STEP: f32 = 15.;

/// Where the screenshot sits on screen: canvas pixels for screenshot
/// pixels, and the part of the canvas it shows (all of it, or its crop).
#[derive(Clone, Copy)]
pub(super) struct Placing {
    /// Where the screenshot's top-left pixel is, cropped off or not.
    pub(super) origin: Xy,
    pub(super) per_pixel: f32,
    /// The part shown, in the canvas.
    pub(super) seen_origin: Xy,
    pub(super) seen_size: Xy,
}

impl Placing {
    /// The screenshot pixel under canvas point `at`, on it or off it.
    pub(super) fn pixel(self, at: Xy) -> (f32, f32) {
        (
            (at.x - self.origin.x) / self.per_pixel,
            (at.y - self.origin.y) / self.per_pixel,
        )
    }

    /// Where screenshot pixel `(x, y)` is in the canvas.
    pub(super) fn at(self, (x, y): (f32, f32)) -> Xy {
        Xy::new(
            self.origin.x + x * self.per_pixel,
            self.origin.y + y * self.per_pixel,
        )
    }
}

/// The rotate button's centre, in the canvas: above the middle of the
/// box's top edge, turned with it.
fn turn_button(shape: &Shape, placing: Placing) -> Xy {
    let frame = shape.frame();
    let top = placing.at((
        (frame[0].0 + frame[1].0) / 2.,
        (frame[0].1 + frame[1].1) / 2.,
    ));
    let (sin, cos) = shape.angle.to_radians().sin_cos();
    // Straight up, turned clockwise by the shape's angle.
    Xy::new(top.x + sin * TURN_OFFSET, top.y - cos * TURN_OFFSET)
}

fn distance(a: Xy, b: Xy) -> f32 {
    (a.x - b.x).hypot(a.y - b.y)
}

impl Shown {
    /// The selected shape, with its place in the marks.
    pub(super) fn selected_shape(&self) -> Option<(usize, &Shape)> {
        let index = self.selected?;
        Some((index, self.marks.marks().get(index)?.as_shape()?))
    }

    /// The selected shape as it looks now: as dragged, during a drag.
    pub(super) fn selection_now(&self) -> Option<Shape> {
        if let Some(Gesture::Edit(editing)) = &self.gesture {
            return Some(editing.edited.clone());
        }
        self.selected_shape().map(|(_, shape)| shape.clone())
    }

    /// The part of the selected shape at canvas point `at`, if any.
    fn grip_at(&self, at: Xy, pixel: (f32, f32), placing: Placing) -> Option<Grip> {
        let (_, shape) = self.selected_shape()?;
        if shape.has_frame() && distance(turn_button(shape, placing), at) <= TURN_SIZE / 2. + 2. {
            return Some(Grip::Turn);
        }
        let handles = shape.handles();
        if let Some(index) = handles
            .iter()
            .position(|&handle| distance(placing.at(handle), at) <= HANDLE_REACH)
        {
            return Some(Grip::Handle(index));
        }
        shape
            .holds(pixel, BODY_REACH / placing.per_pixel)
            .then_some(Grip::Body)
    }

    /// The topmost shape at screenshot pixel `pixel`, if any.
    fn shape_at(&self, pixel: (f32, f32), per_pixel: f32) -> Option<usize> {
        self.marks.marks().iter().rposition(|mark| {
            mark.as_shape()
                .is_some_and(|shape| shape.holds(pixel, BODY_REACH / per_pixel))
        })
    }
}

impl MainWindow {
    /// Pick up the shape at `index`, or let the selected one go. The
    /// drawing is made again without the selected shape, which is painted
    /// over it instead, as it changes.
    pub(super) fn select(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(shown) = &mut self.shown else {
            return;
        };
        if shown.selected == index {
            return;
        }
        shown.selected = index;
        self.shape_context = None;
        self.redraw(window, cx);
    }

    /// Pick up a shape that was there before, and make its colours and
    /// size the Shapes bar's, so the bar shows them and changes them.
    fn select_existing(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.select(Some(index), window, cx);
        let Some(shown) = &self.shown else {
            return;
        };
        let Some((_, shape)) = shown.selected_shape() else {
            return;
        };
        let current = self.shape_style();
        let sizes = ShapeStyle::SIZES;
        let size = (shape.width / shown.shot.scale.unwrap_or(shown.scale))
            .round()
            .clamp(*sizes.start(), *sizes.end());
        let style = ShapeStyle {
            // The bar never offers an emoji as the next shape.
            kind: match shape.kind {
                ShapeKind::Emoji(_) => current.kind,
                kind => kind,
            },
            outline: shape.outline,
            fill: if shape.kind.fills() {
                shape.fill
            } else {
                current.fill
            },
            size,
        };
        self.change(cx, |settings| settings.set_shape_style(style));
    }

    /// A press with the Shapes tool or Select: on the selected shape, start
    /// changing it; with Select, pick up the shape there and start moving
    /// it. Otherwise the selected shape is let go, and `None` leaves the
    /// press to the tool.
    pub(super) fn press_selection(
        &mut self,
        at: Xy,
        pixel: (f32, f32),
        placing: Placing,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Gesture> {
        let shown = self.shown.as_ref()?;
        let grip = shown.grip_at(at, pixel, placing);
        let index = match grip {
            Some(_) => shown.selected?,
            None if self.hand == Some(Hand::Select) => {
                match shown.shape_at(pixel, placing.per_pixel) {
                    Some(index) => {
                        self.select_existing(index, window, cx);
                        index
                    }
                    None => {
                        self.select(None, window, cx);
                        return None;
                    }
                }
            }
            None => {
                self.select(None, window, cx);
                return None;
            }
        };
        let shape = self.shown.as_ref()?.marks.marks()[index]
            .as_shape()?
            .clone();
        Some(Gesture::Edit(Editing {
            index,
            original: shape.clone(),
            edited: shape,
            grip: grip.unwrap_or(Grip::Body),
            from: pixel,
            from_at: at,
            moved: false,
        }))
    }

    /// A right-click with the Shapes tool or Select: pick up the shape
    /// there and offer its menu.
    pub(super) fn open_shape_context(
        &mut self,
        at: Xy,
        pixel: (f32, f32),
        placing: Placing,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(shown) = &self.shown else {
            return;
        };
        let on_selected = shown.grip_at(at, pixel, placing).is_some();
        let index = if on_selected {
            shown.selected
        } else {
            shown.shape_at(pixel, placing.per_pixel)
        };
        let Some(index) = index else {
            self.shape_context = None;
            cx.notify();
            return;
        };
        if !on_selected {
            self.select_existing(index, window, cx);
        }
        self.close_flyouts();
        self.shape_context = Some(ShapeContext { at, highlighted: 0 });
        cx.notify();
    }

    /// Change the selected shape, as one edit of kind `how`.
    fn edit_selection(
        &mut self,
        how: Edit,
        cx: &mut Context<Self>,
        change: impl FnOnce(&Shape) -> Shape,
    ) {
        let Some(shown) = &mut self.shown else {
            return;
        };
        let Some((index, shape)) = shown.selected_shape() else {
            return;
        };
        let changed = change(shape);
        if changed == *shape {
            return;
        }
        shown.marks.edit(index, Mark::Shape(changed), how);
        cx.notify();
    }

    /// Take the selected shape off.
    fn delete_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = &mut self.shown else {
            return;
        };
        let Some(index) = shown.selected.take() else {
            return;
        };
        shown.marks.erase(index, false);
        self.shape_context = None;
        self.redraw(window, cx);
    }

    /// The Shapes bar's colours, opacity or size changed from `before` to
    /// `after`: the selected shape takes on what changed.
    pub(super) fn restyle_selection(
        &mut self,
        before: ShapeStyle,
        after: ShapeStyle,
        cx: &mut Context<Self>,
    ) {
        let Some(shown) = &self.shown else {
            return;
        };
        let width = shown.width_for(after.size);
        self.edit_selection(Edit::Style, cx, |shape| {
            let mut shape = shape.clone();
            if after.outline != before.outline {
                shape.outline = after.outline;
            }
            if after.fill != before.fill && shape.kind.fills() {
                shape.fill = after.fill;
            }
            if after.size != before.size {
                shape.width = width;
            }
            shape
        });
    }

    /// The pointer moved during a drag on the selected shape, to screenshot
    /// pixel `pixel`, canvas point `at`.
    pub(super) fn drag_selection(editing: &mut Editing, pixel: (f32, f32), at: Xy, shift: bool) {
        editing.moved |= (at.x - editing.from_at.x).abs() + (at.y - editing.from_at.y).abs()
            > crate::markup::LEAST_DRAG;
        if !editing.moved {
            return;
        }
        let original = &editing.original;
        editing.edited = match editing.grip {
            Grip::Body => original.moved((pixel.0 - editing.from.0, pixel.1 - editing.from.1)),
            Grip::Handle(index) => original.with_handle(index, pixel),
            Grip::Turn => {
                let center = original.center();
                let mut angle = (pixel.1 - center.1).atan2(pixel.0 - center.0).to_degrees() + 90.;
                if shift {
                    angle = (angle / TURN_STEP).round() * TURN_STEP;
                }
                Shape {
                    angle: angle.rem_euclid(360.),
                    ..original.clone()
                }
            }
        };
    }

    /// The drag on the selected shape is over: keep what it did, as one
    /// edit.
    pub(super) fn finish_editing(&mut self, editing: Editing, cx: &mut Context<Self>) {
        if editing.moved
            && let Some(shown) = &mut self.shown
        {
            shown
                .marks
                .edit(editing.index, Mark::Shape(editing.edited), Edit::Once);
        }
        cx.notify();
    }

    /// A key with a shape selected and no menu open: arrows move it,
    /// Shift+arrows resize it, Alt+Left and Alt+Right turn it, Delete takes
    /// it off; in its right-click menu, arrows and Enter. Returns whether
    /// the key was used.
    pub(super) fn on_selection_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = keystroke.key.as_str();
        if let Some(context) = &mut self.shape_context {
            let count = ContextItem::ALL.len();
            match key {
                "up" => context.highlighted = (context.highlighted + count - 1) % count,
                "down" | "tab" => context.highlighted = (context.highlighted + 1) % count,
                "enter" | "space" => {
                    let item = ContextItem::ALL[context.highlighted];
                    self.choose_context(item, window, cx);
                }
                "escape" => self.shape_context = None,
                _ => return false,
            }
            cx.notify();
            return true;
        }
        let selected = self.shown.as_ref().is_some_and(|s| s.selected.is_some());
        let menus = self.menu.is_some() || self.flyout.is_some() || self.shape_menu.is_some();
        if !selected || menus || keystroke.modifiers.control {
            return false;
        }
        let (alt, shift) = (keystroke.modifiers.alt, keystroke.modifiers.shift);
        let step = |positive: &str, negative: &str| {
            if key == positive {
                1.
            } else if key == negative {
                -1.
            } else {
                0.
            }
        };
        let (across, down) = (step("right", "left"), step("down", "up"));
        match key {
            "delete" | "backspace" => self.delete_selection(window, cx),
            "left" | "right" if alt => {
                self.edit_selection(Edit::Nudge, cx, |shape| shape.turned(across * TURN_STEP))
            }
            // Up grows, as Right does.
            "left" | "right" | "up" | "down" if shift => {
                self.edit_selection(Edit::Nudge, cx, |shape| {
                    shape.grown(across * GROW_STEP, -down * GROW_STEP)
                })
            }
            "left" | "right" | "up" | "down" if !alt => {
                self.edit_selection(Edit::Nudge, cx, |shape| shape.moved((across, down)))
            }
            _ => return false,
        }
        true
    }

    fn choose_context(&mut self, item: ContextItem, window: &mut Window, cx: &mut Context<Self>) {
        self.shape_context = None;
        match item {
            ContextItem::Delete => self.delete_selection(window, cx),
            ContextItem::TurnRight => {
                self.edit_selection(Edit::Once, cx, |shape| shape.turned(90.))
            }
            ContextItem::TurnLeft => {
                self.edit_selection(Edit::Once, cx, |shape| shape.turned(-90.))
            }
            ContextItem::TurnHalf => {
                self.edit_selection(Edit::Once, cx, |shape| shape.turned(180.))
            }
        }
        cx.notify();
    }

    /// The selected shape's box, handles and rotate button, over it.
    pub(super) fn selection_overlay(&self, shown: &Shown, placing: Placing) -> Vec<AnyElement> {
        let Some(shape) = shown.selection_now() else {
            return Vec::new();
        };
        let mut overlay = Vec::new();
        if shape.has_frame() {
            let frame = shape.frame().map(|corner| placing.at(corner));
            overlay.push(frame_lines(frame).into_any_element());
            let at = turn_button(&shape, placing);
            overlay.push(
                div()
                    .id("turn-handle")
                    .aria_label("Rotate")
                    .test_support()
                    .absolute()
                    .left(px(at.x - TURN_SIZE / 2.))
                    .top(px(at.y - TURN_SIZE / 2.))
                    .size(px(TURN_SIZE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(rgb(0x2C2C2C))
                    .border_1()
                    .border_color(border())
                    .shadow_md()
                    .child(Icon::new(IconName::RotateCw).size(px(14.)))
                    .into_any_element(),
            );
        }
        for (i, handle) in shape.handles().into_iter().enumerate() {
            let at = placing.at(handle);
            overlay.push(
                div()
                    .id(SharedString::from(format!("handle-{i}")))
                    .test_support()
                    .absolute()
                    .left(px(at.x - HANDLE_SIZE / 2.))
                    .top(px(at.y - HANDLE_SIZE / 2.))
                    .size(px(HANDLE_SIZE))
                    .rounded_full()
                    .bg(gpui_kit::white())
                    .border_1()
                    .border_color(gpui_kit::black().opacity(0.6))
                    .into_any_element(),
            );
        }
        overlay
    }

    /// The right-click menu, where it opened.
    pub(super) fn shape_context_menu(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let context = self.shape_context.as_ref()?;
        let items = ContextItem::ALL.into_iter().enumerate().map(|(i, item)| {
            let highlighted = i == context.highlighted;
            div()
                .id(item.id())
                .role(Role::MenuItem)
                .aria_label(item.label())
                .test_support()
                .flex()
                .items_center()
                .gap_2p5()
                .h(px(34.))
                .px_3()
                .rounded_md()
                .text_sm()
                .whitespace_nowrap()
                .when(highlighted, |d| d.bg(rgb(0x383838)))
                .hover(|s| s.bg(rgb(0x383838)))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.choose_context(item, window, cx)
                }))
                .child(
                    Icon::new(match item {
                        ContextItem::Delete => IconName::Trash,
                        ContextItem::TurnLeft => IconName::RotateCcw,
                        _ => IconName::RotateCw,
                    })
                    .size(px(16.)),
                )
                .child(item.label())
                .when(item == ContextItem::Delete, |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(crate::palette::muted())
                            .child("Del"),
                    )
                })
        });
        Some(
            deferred(
                div()
                    .id("shape-context")
                    .role(Role::Menu)
                    .test_support()
                    .absolute()
                    .left(px(context.at.x))
                    .top(px(context.at.y))
                    .p(px(5.))
                    .rounded_lg()
                    .bg(rgb(0x2C2C2C))
                    .border_1()
                    .border_color(border())
                    .shadow_lg()
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .children(items),
            )
            .with_priority(1),
        )
    }
}

/// A box's outline through its four `corners`, canvas pixels: a light line
/// over a dark one, to show on any screenshot.
fn frame_lines(corners: [Xy; 4]) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window, _| {
            let to_window = |at: Xy| point(bounds.origin.x + px(at.x), bounds.origin.y + px(at.y));
            let lines = [
                (3., gpui_kit::black().opacity(0.6)),
                (1., gpui_kit::white().opacity(0.9)),
            ];
            for (width, color) in lines {
                let mut path = PathBuilder::stroke(px(width));
                path.move_to(to_window(corners[0]));
                for &corner in &corners[1..] {
                    path.line_to(to_window(corner));
                }
                path.close();
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            }
        },
    )
    .absolute()
    .size_full()
}
