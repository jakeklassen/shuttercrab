//! Cropping the screenshot shown, as Snipping Tool's Image crop. C, or the
//! crop button, shows the whole screenshot with a frame on the part kept
//! and the rest dimmed. Drag a corner or an edge to resize the frame, the
//! inside to move it; it stays on the screenshot, 48 pixels square at
//! least. Tab goes between the inside, the top-left corner and the
//! bottom-right corner, and arrows move or resize by 5 pixels. Enter
//! applies, Escape cancels. Nothing is lost: the whole screenshot stays, so
//! opening crop again can widen it, and undo takes a crop back. Unlike
//! Snipping Tool, the frame's size shows as it changes.

use super::{
    MainWindow,
    canvas::{Gesture, Shown},
    selection::Placing,
};
use crate::{
    markup::Region,
    palette::{border, coral, muted},
    shot_view::Xy,
};
use gpui_kit::{
    AnyElement, App, Context, CursorStyle, InteractiveElement as _, IntoElement, KeyBinding,
    Keystroke, MouseButton, MouseUpEvent, ParentElement as _, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, actions,
    assets::IconName, component::Icon, div, px, rgb,
};

/// The least the frame keeps, screenshot pixels each way: Snipping Tool's.
const LEAST: f32 = 48.;

/// How far an arrow key moves or resizes the frame, screenshot pixels.
const STEP: f32 = 5.;

/// How near a corner or an edge a press takes it, logical pixels on screen.
const REACH: f32 = 10.;

/// The handles' look, logical pixels: bars this long and thick, just
/// outside the frame.
const BAR: f32 = 24.;
const THICK: f32 = 4.;

/// The part of the screenshot a crop keeps, screenshot pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Frame {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

/// Which edges a drag or a key moves: none of them moves the whole frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Grip {
    left: bool,
    top: bool,
    right: bool,
    bottom: bool,
}

impl Grip {
    const INSIDE: Grip = Grip {
        left: false,
        top: false,
        right: false,
        bottom: false,
    };
    const TOP_LEFT: Grip = Grip {
        left: true,
        top: true,
        right: false,
        bottom: false,
    };
    const BOTTOM_RIGHT: Grip = Grip {
        left: false,
        top: false,
        right: true,
        bottom: true,
    };

    /// The parts Tab goes between, in order.
    const FOCUSABLE: [Grip; 3] = [Grip::INSIDE, Grip::TOP_LEFT, Grip::BOTTOM_RIGHT];
}

impl Frame {
    fn of(region: Region) -> Frame {
        Frame {
            left: region.x as f32,
            top: region.y as f32,
            right: (region.x + region.width) as f32,
            bottom: (region.y + region.height) as f32,
        }
    }

    /// The pixels it keeps, whole ones.
    pub(super) fn region(self) -> Region {
        let (left, top) = (self.left.round() as u32, self.top.round() as u32);
        Region {
            x: left,
            y: top,
            width: self.right.round() as u32 - left,
            height: self.bottom.round() as u32 - top,
        }
    }

    /// The frame with `grip`'s edges moved `by` screenshot pixels: each
    /// stays on the `size` screenshot, `LEAST` from the edge across. With
    /// no edges gripped, the whole frame moves, as far as the screenshot
    /// allows.
    fn dragged(self, grip: Grip, by: (f32, f32), (width, height): (f32, f32)) -> Frame {
        if grip == Grip::INSIDE {
            let dx = by.0.clamp(-self.left, width - self.right);
            let dy = by.1.clamp(-self.top, height - self.bottom);
            return Frame {
                left: self.left + dx,
                top: self.top + dy,
                right: self.right + dx,
                bottom: self.bottom + dy,
            };
        }
        let mut frame = self;
        if grip.left {
            frame.left = (self.left + by.0).clamp(0., (self.right - LEAST).max(0.));
        }
        if grip.right {
            frame.right = (self.right + by.0).clamp((self.left + LEAST).min(width), width);
        }
        if grip.top {
            frame.top = (self.top + by.1).clamp(0., (self.bottom - LEAST).max(0.));
        }
        if grip.bottom {
            frame.bottom = (self.bottom + by.1).clamp((self.top + LEAST).min(height), height);
        }
        frame
    }

    /// The part of the frame at canvas point `at`, if any: a corner, an
    /// edge, or the inside.
    fn grip_at(self, at: Xy, placing: Placing) -> Option<Grip> {
        let top_left = placing.at((self.left, self.top));
        let bottom_right = placing.at((self.right, self.bottom));
        let near = |a: f32, b: f32| (a - b).abs() <= REACH;
        let across = top_left.x - REACH..=bottom_right.x + REACH;
        let down = top_left.y - REACH..=bottom_right.y + REACH;
        if !(across.contains(&at.x) && down.contains(&at.y)) {
            return None;
        }
        // A frame too small on screen for both edges: the nearer one.
        let left =
            near(at.x, top_left.x) && (at.x - top_left.x).abs() <= (at.x - bottom_right.x).abs();
        let top =
            near(at.y, top_left.y) && (at.y - top_left.y).abs() <= (at.y - bottom_right.y).abs();
        Some(Grip {
            left,
            top,
            right: near(at.x, bottom_right.x) && !left,
            bottom: near(at.y, bottom_right.y) && !top,
        })
    }
}

/// Crop mode: the frame, what the crop was before, and the part the keys
/// work on.
pub(super) struct Cropping {
    pub(super) frame: Frame,
    focus: Grip,
}

/// A drag on the frame: which part, where it started (screenshot pixels),
/// and the frame then.
#[derive(Clone, Copy)]
pub(super) struct CropDrag {
    grip: Grip,
    from: (f32, f32),
    start: Frame,
}

impl Shown {
    fn size_f32(&self) -> (f32, f32) {
        let (width, height) = self.shot.size();
        (width as f32, height as f32)
    }
}

actions!(crop, [NextGrip, PreviousGrip]);

/// The key context crop mode adds, for its Tab and Shift+Tab: the window's
/// root otherwise takes those to move the focus.
pub(super) const CONTEXT: &str = "Crop";

/// Tab and Shift+Tab while cropping. Bound for each window: GPUI keeps a
/// binding until the application ends, and a test's application is its
/// own.
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", NextGrip, Some(CONTEXT)),
        KeyBinding::new("shift-tab", PreviousGrip, Some(CONTEXT)),
    ]);
}

impl Grip {
    /// The pointer over this part: a resize arrow along the way it moves,
    /// or a hand over the inside (closed while dragging it).
    fn cursor(self, dragging: bool) -> CursorStyle {
        match (self.left || self.right, self.top || self.bottom) {
            (true, true) if self.left == self.top => CursorStyle::ResizeUpLeftDownRight,
            (true, true) => CursorStyle::ResizeUpRightDownLeft,
            (true, false) => CursorStyle::ResizeLeftRight,
            (false, true) => CursorStyle::ResizeUpDown,
            (false, false) if dragging => CursorStyle::ClosedHand,
            (false, false) => CursorStyle::OpenHand,
        }
    }
}

impl MainWindow {
    /// The pointer while cropping, over a canvas of size `canvas`: what
    /// dragging the part of the frame under it, or being dragged, does.
    /// `None` when not cropping.
    pub(super) fn crop_cursor(&self, shown: &Shown, canvas: Xy) -> Option<CursorStyle> {
        let cropping = shown.cropping.as_ref()?;
        if let Some(Gesture::Crop(drag)) = &shown.gesture {
            return Some(drag.grip.cursor(true));
        }
        let placing = shown.placing(canvas);
        let grip = self
            .pointer
            .and_then(|at| cropping.frame.grip_at(at, placing));
        Some(grip.map_or(CursorStyle::Arrow, |grip| grip.cursor(false)))
    }

    /// Tab or Shift+Tab while cropping: the next part for the arrow keys,
    /// or the one before.
    pub(super) fn step_crop_focus(&mut self, back: bool, cx: &mut Context<Self>) {
        if let Some(cropping) = self.shown.as_mut().and_then(|s| s.cropping.as_mut()) {
            let parts = Grip::FOCUSABLE;
            let at = parts.iter().position(|g| *g == cropping.focus).unwrap_or(0);
            let next = if back { at + parts.len() - 1 } else { at + 1 };
            cropping.focus = parts[next % parts.len()];
            cx.notify();
        }
    }

    pub(super) fn is_cropping(&self) -> bool {
        self.shown.as_ref().is_some_and(|s| s.cropping.is_some())
    }

    /// Open crop: the whole screenshot, the frame on its crop (or all of
    /// it), and nothing else in hand.
    pub(super) fn start_crop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown.is_none() || self.is_cropping() {
            return;
        }
        self.close_flyouts();
        self.menu = None;
        self.select(None, window, cx);
        self.hand = None;
        let Some(shown) = &mut self.shown else {
            return;
        };
        let kept = shown.marks.crop().unwrap_or(shown.whole());
        shown.cropping = Some(Cropping {
            frame: Frame::of(kept),
            focus: Grip::INSIDE,
        });
        cx.notify();
    }

    /// Keep the frame's part, as one change undo takes back; all of it is
    /// no crop.
    pub(super) fn apply_crop(&mut self, cx: &mut Context<Self>) {
        let Some(shown) = &mut self.shown else {
            return;
        };
        let Some(cropping) = shown.cropping.take() else {
            return;
        };
        let kept = cropping.frame.region();
        let crop = (kept != shown.whole()).then_some(kept);
        shown.marks.set_crop(crop);
        cx.notify();
    }

    /// Leave crop with the crop as it was.
    pub(super) fn cancel_crop(&mut self, cx: &mut Context<Self>) {
        if let Some(shown) = &mut self.shown
            && shown.cropping.take().is_some()
        {
            cx.notify();
        }
    }

    /// A press while cropping, at canvas point `at`: take hold of the
    /// frame there, if any.
    pub(super) fn press_crop(&self, shown: &Shown, at: Xy, placing: Placing) -> Option<Gesture> {
        let cropping = shown.cropping.as_ref()?;
        let grip = cropping.frame.grip_at(at, placing)?;
        Some(Gesture::Crop(CropDrag {
            grip,
            from: placing.pixel(at),
            start: cropping.frame,
        }))
    }

    /// The pointer moved, to screenshot pixel `pixel`, during a drag on the
    /// frame.
    pub(super) fn drag_crop(shown: &mut Shown, drag: &CropDrag, pixel: (f32, f32)) {
        let size = shown.size_f32();
        if let Some(cropping) = &mut shown.cropping {
            let by = (pixel.0 - drag.from.0, pixel.1 - drag.from.1);
            cropping.frame = drag.start.dragged(drag.grip, by, size);
        }
    }

    /// A key while cropping: Tab goes between the inside and two corners,
    /// arrows move or resize, Enter applies, Escape cancels. Only zooming
    /// and Space for moving the screenshot work besides. Returns whether
    /// the key was used.
    pub(super) fn on_crop_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        if !self.is_cropping() {
            return false;
        }
        let key = keystroke.key.as_str();
        if keystroke.modifiers.control {
            return !matches!(key, "=" | "+" | "-" | "0" | "1");
        }
        let by = match key {
            "left" => (-STEP, 0.),
            "right" => (STEP, 0.),
            "up" => (0., -STEP),
            "down" => (0., STEP),
            "enter" => {
                self.apply_crop(cx);
                return true;
            }
            "escape" => {
                self.cancel_crop(cx);
                return true;
            }
            "space" => return false,
            _ => return true,
        };
        if let Some(shown) = &mut self.shown {
            let size = shown.size_f32();
            if let Some(cropping) = &mut shown.cropping {
                cropping.frame = cropping.frame.dragged(cropping.focus, by, size);
            }
        }
        cx.notify();
        true
    }

    /// Crop mode over the screenshot: the outside dimmed, the frame with
    /// its handles, its size, and Apply and Cancel.
    pub(super) fn crop_overlay(
        &self,
        shown: &Shown,
        placing: Placing,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(cropping) = &shown.cropping else {
            return Vec::new();
        };
        let frame = cropping.frame;
        let top_left = placing.at((frame.left, frame.top));
        let bottom_right = placing.at((frame.right, frame.bottom));
        let (seen, size) = (placing.seen_origin, placing.seen_size);
        let (w, h) = (bottom_right.x - top_left.x, bottom_right.y - top_left.y);
        let dim = |left: f32, top: f32, width: f32, height: f32| {
            div()
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(width.max(0.)))
                .h(px(height.max(0.)))
                .bg(gpui_kit::black().opacity(0.6))
                .into_any_element()
        };
        let mut overlay = vec![
            // Above, below, then either side.
            dim(seen.x, seen.y, size.x, top_left.y - seen.y),
            dim(
                seen.x,
                bottom_right.y,
                size.x,
                seen.y + size.y - bottom_right.y,
            ),
            dim(seen.x, top_left.y, top_left.x - seen.x, h),
            dim(
                bottom_right.x,
                top_left.y,
                seen.x + size.x - bottom_right.x,
                h,
            ),
            div()
                .id("crop-frame")
                .test_support()
                .absolute()
                .left(px(top_left.x))
                .top(px(top_left.y))
                .w(px(w))
                .h(px(h))
                .border_1()
                .border_color(if cropping.focus == Grip::INSIDE {
                    coral()
                } else {
                    gpui_kit::white()
                })
                .into_any_element(),
        ];
        overlay.extend(handles(top_left, bottom_right, cropping.focus));
        let (l, b) = (top_left.x, bottom_right.y);
        let kept = frame.region();
        let readout = SharedString::from(format!("{} × {}", kept.width, kept.height));
        overlay.push(
            div()
                .id("crop-size")
                .aria_label(readout.clone())
                .test_support()
                .absolute()
                .left(px(l))
                .top(px(b + THICK + 6.))
                .px_1p5()
                .py_0p5()
                .rounded_md()
                .bg(rgb(0x2C2C2C))
                .border_1()
                .border_color(border())
                .text_xs()
                .child(readout)
                .into_any_element(),
        );
        overlay.push(Self::crop_bar(cx).into_any_element());
        overlay
    }

    /// The toolbar's crop button, after the drawing tools.
    pub(super) fn crop_button(cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .id("tool-crop")
            .role(Role::Button)
            .aria_label("Crop (C)")
            .test_support()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .size(px(40.))
            .rounded_md()
            .hover(|s| s.bg(crate::palette::hover()))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| this.start_crop(window, cx)),
            )
            .child(
                div()
                    .absolute()
                    .top(px(1.))
                    .right(px(3.))
                    .text_size(px(9.))
                    .text_color(muted())
                    .child("C"),
            )
            .child(Icon::new(IconName::Crop).size(px(18.)))
    }

    /// Apply and Cancel, over the top of the screenshot.
    fn crop_bar(cx: &mut Context<Self>) -> impl IntoElement {
        let button = |id: &'static str, label: &'static str, key: &'static str, icon: IconName| {
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(SharedString::from(format!("{label} ({key})")))
                .test_support()
                .flex()
                .items_center()
                .gap_2()
                .h(px(36.))
                .px_3()
                .rounded_md()
                .hover(|s| s.bg(rgb(0x383838)))
                .cursor_pointer()
                .child(Icon::new(icon).size(px(16.)))
                .child(div().text_sm().child(label))
                .child(div().text_xs().text_color(muted()).child(key))
        };
        div()
            .absolute()
            .top(px(6.))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .id("crop-bar")
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
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        button("crop-apply", "Apply", "Enter", IconName::Check).on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseUpEvent, _, cx| this.apply_crop(cx)),
                        ),
                    )
                    .child(
                        button("crop-cancel", "Cancel", "Esc", IconName::X).on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseUpEvent, _, cx| this.cancel_crop(cx)),
                        ),
                    ),
            )
    }
}

/// The frame's handles, from its corners on screen: an L-shaped bracket
/// at each corner and a bar in the middle of each edge, just outside it,
/// as Snipping Tool's. The corner the keys work on is coral.
fn handles(top_left: Xy, bottom_right: Xy, focus: Grip) -> Vec<AnyElement> {
    let bar = |left: f32, top: f32, width: f32, height: f32, focused: bool| {
        div()
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(width))
            .h(px(height))
            .rounded(px(1.))
            .bg(if focused { coral() } else { gpui_kit::white() })
            .into_any_element()
    };
    let (l, t, r, b) = (top_left.x, top_left.y, bottom_right.x, bottom_right.y);
    let (top_left_on, bottom_right_on) = (focus == Grip::TOP_LEFT, focus == Grip::BOTTOM_RIGHT);
    vec![
        // The corners' brackets.
        bar(l - THICK, t - THICK, BAR, THICK, top_left_on),
        bar(l - THICK, t - THICK, THICK, BAR, top_left_on),
        bar(r - BAR + THICK, t - THICK, BAR, THICK, false),
        bar(r, t - THICK, THICK, BAR, false),
        bar(l - THICK, b, BAR, THICK, false),
        bar(l - THICK, b - BAR + THICK, THICK, BAR, false),
        bar(r - BAR + THICK, b, BAR, THICK, bottom_right_on),
        bar(r, b - BAR + THICK, THICK, BAR, bottom_right_on),
        // The edges' bars, in their middles.
        bar((l + r - BAR) / 2., t - THICK, BAR, THICK, false),
        bar((l + r - BAR) / 2., b, BAR, THICK, false),
        bar(l - THICK, (t + b - BAR) / 2., THICK, BAR, false),
        bar(r, (t + b - BAR) / 2., THICK, BAR, false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (f32, f32) = (400., 300.);

    fn frame(left: f32, top: f32, right: f32, bottom: f32) -> Frame {
        Frame {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn a_corner_resizes_two_edges_and_stops_at_the_least_size() {
        let start = frame(100., 100., 300., 200.);
        let corner = start.dragged(Grip::TOP_LEFT, (-20., 30.), SIZE);
        assert_eq!(corner, frame(80., 130., 300., 200.));
        // Past the opposite edge: it stops 48 short, and never flips.
        let squeezed = start.dragged(Grip::BOTTOM_RIGHT, (-500., -500.), SIZE);
        assert_eq!(squeezed, frame(100., 100., 148., 148.));
        // Never off the screenshot.
        let out = start.dragged(Grip::TOP_LEFT, (-500., -500.), SIZE);
        assert_eq!(out, frame(0., 0., 300., 200.));
    }

    #[test]
    fn the_inside_moves_the_frame_and_keeps_it_on_the_screenshot() {
        let start = frame(100., 100., 300., 200.);
        assert_eq!(
            start.dragged(Grip::INSIDE, (50., -20.), SIZE),
            frame(150., 80., 350., 180.)
        );
        assert_eq!(
            start.dragged(Grip::INSIDE, (500., 500.), SIZE),
            frame(200., 200., 400., 300.)
        );
    }

    #[test]
    fn each_part_shows_the_way_it_drags() {
        let grip = |left, top, right, bottom| Grip {
            left,
            top,
            right,
            bottom,
        };
        let cursors = [
            (
                grip(true, true, false, false),
                CursorStyle::ResizeUpLeftDownRight,
            ),
            (
                grip(false, false, true, true),
                CursorStyle::ResizeUpLeftDownRight,
            ),
            (
                grip(false, true, true, false),
                CursorStyle::ResizeUpRightDownLeft,
            ),
            (
                grip(true, false, false, true),
                CursorStyle::ResizeUpRightDownLeft,
            ),
            (
                grip(true, false, false, false),
                CursorStyle::ResizeLeftRight,
            ),
            (
                grip(false, false, true, false),
                CursorStyle::ResizeLeftRight,
            ),
            (grip(false, true, false, false), CursorStyle::ResizeUpDown),
            (grip(false, false, false, true), CursorStyle::ResizeUpDown),
            (Grip::INSIDE, CursorStyle::OpenHand),
        ];
        for (grip, cursor) in cursors {
            assert_eq!(grip.cursor(false), cursor, "{grip:?}");
        }
        assert_eq!(Grip::INSIDE.cursor(true), CursorStyle::ClosedHand);
    }

    #[test]
    fn the_frame_keeps_whole_pixels() {
        let kept = frame(10.4, 20.6, 110.4, 70.5).region();
        assert_eq!(
            kept,
            Region {
                x: 10,
                y: 21,
                width: 100,
                height: 50
            }
        );
    }
}
