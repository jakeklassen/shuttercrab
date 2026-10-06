//! The screenshot in the main window: where it sits (zoom and position),
//! moving it about, and drawing on it with the pen, the highlighter and the
//! shapes.
//!
//! Finished strokes are drawn onto a copy of the screenshot off the main
//! thread ([`markup::draw`]), the same way Copy and Save draw them, so what
//! the window shows is what they give. Until that copy is ready, and while
//! a stroke is being drawn, pen strokes are painted over it directly: they
//! cover what is under them, so that looks the same. GPUI cannot multiply
//! colours, so a highlighter stroke is drawn onto just the patch of the
//! screenshot it covers, as the stroke grows, and shown over it.

use super::{
    FOOTER_HEIGHT, Hand, MainWindow, Shot, TOOLBAR_HEIGHT,
    crop::{CropDrag, Cropping},
    selection::{Editing, Grip, Placing},
};
use crate::{
    cursors,
    markup::{
        self, Drawing, Emoji, Figure, Ink, Mark, Marks, Region, Shape, ShapeKind, Stroke, Tool,
    },
    palette::border,
    pixels, popup,
    shot_view::{ShotView, Xy},
};
use gpui_kit::{
    AnyElement, Bounds, ContentMask, Context, Corners, CursorStyle, InteractiveElement as _,
    IntoElement, KeyUpEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement as _, PathBuilder, PathStyle, Pixels, Point, RenderImage, ScrollWheelEvent,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, canvas, div, img,
    point, px, rgb, size,
};
use lyon_tessellation::{LineCap, LineJoin, StrokeOptions};
use shuttercrab_platform::cursor::{Cursor, CursorOver};
use std::{cell::RefCell, sync::Arc};

/// The screenshot shown, how it sits in the canvas, and the marks on it.
pub(super) struct Shown {
    pub(super) shot: Shot,
    pub(super) view: ShotView,
    /// The window's scale `view` was made for: its sizes are logical.
    pub(super) scale: f32,
    pub(super) marks: Marks,
    /// The screenshot with marks drawn on: shown in its place. Drawn off
    /// the main thread after each change, so it can lag behind `marks`.
    drawn: Option<Drawn>,
    /// The highlighter stroke being drawn, multiplied onto the part of the
    /// screenshot it covers.
    patch: Option<Patch>,
    /// A patch is being drawn; the next waits for it.
    patching: bool,
    pub(super) gesture: Option<Gesture>,
    /// The shape picked up to change, by its place in the marks: left out
    /// of the drawing and painted over it.
    pub(super) selected: Option<usize>,
    /// Emoji drawn for the screen, as the canvas shows them.
    emoji_pictures: RefCell<EmojiPictures>,
    /// The part of the screenshot `view` was fitted to: its crop, or all
    /// of it.
    view_area: Region,
    /// Crop mode, while open.
    pub(super) cropping: Option<Cropping>,
}

/// Emoji drawn for the screen, each kept while it looks the same there, so
/// it is drawn once; those no longer shown wait for the next frame to be
/// let go of.
#[derive(Default)]
struct EmojiPictures {
    kept: Vec<(Picture, Arc<RenderImage>)>,
    stale: Vec<Arc<RenderImage>>,
}

/// How an emoji looks on screen: which, how many device pixels across,
/// and how far turned, in tenths of a degree.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Picture {
    emoji: Emoji,
    side: u32,
    angle: i32,
}

impl EmojiPictures {
    /// The pictures `wanted` this frame, drawing those not kept; the
    /// kept ones not wanted go stale.
    fn for_frame(&mut self, wanted: &[Option<Picture>]) -> Vec<Option<Arc<RenderImage>>> {
        let mut kept: Vec<(Picture, Arc<RenderImage>)> = Vec::new();
        let mut pictures = Vec::with_capacity(wanted.len());
        for picture in wanted {
            let Some(picture) = *picture else {
                pictures.push(None);
                continue;
            };
            let known = kept.iter().chain(&self.kept).find(|(p, _)| *p == picture);
            let image = match known {
                Some((_, image)) => image.clone(),
                None => {
                    let angle = picture.angle as f32 / 10.;
                    let (bgra, span) =
                        markup::emoji_image(picture.emoji, picture.side as f32, angle);
                    pixels::bgra_image(bgra, span, span)
                }
            };
            if !kept.iter().any(|(p, _)| *p == picture) {
                kept.push((picture, image.clone()));
            }
            pictures.push(Some(image));
        }
        for (picture, image) in std::mem::replace(&mut self.kept, kept) {
            if !self.kept.iter().any(|(p, _)| *p == picture) {
                self.stale.push(image);
            }
        }
        pictures
    }

    /// Let go of the pictures no longer shown.
    fn drop_stale(&mut self, window: &mut Window) {
        for image in self.stale.drain(..) {
            let _ = window.drop_image(image);
        }
    }
}

/// The screenshot with `marks` drawn on it.
struct Drawn {
    image: Arc<RenderImage>,
    marks: Vec<Mark>,
}

/// A highlighter stroke drawn onto just `region` of the screenshot,
/// multiplied as it will be: shown over the screenshot while the stroke is
/// drawn, and until the whole drawing has it.
struct Patch {
    image: Arc<RenderImage>,
    region: Region,
    /// The stroke's place in the marks once finished.
    index: usize,
}

/// What a drag on the canvas is doing.
pub(super) enum Gesture {
    /// Moving the screenshot; where the pointer last was, in the canvas.
    Pan(Xy),
    /// Drawing a stroke.
    Draw(Drawing),
    /// Erasing: where the eraser last was, in screenshot pixels, and
    /// whether this drag has taken anything yet.
    Erase { last: (f32, f32), erased: bool },
    /// Drawing a shape: where the press was, in the canvas, and whether the
    /// drag has gone far enough to be one.
    Shape { shape: Shape, from: Xy, drawn: bool },
    /// Changing the selected shape.
    Edit(Editing),
    /// Resizing or moving the crop frame.
    Crop(CropDrag),
}

impl Shown {
    pub(super) fn new(shot: Shot, scale: f32) -> Self {
        let (width, height) = shot.size();
        let whole = Region {
            x: 0,
            y: 0,
            width,
            height,
        };
        Self {
            view: view_for(whole, scale),
            view_area: whole,
            shot,
            scale,
            marks: Marks::default(),
            drawn: None,
            patch: None,
            patching: false,
            gesture: None,
            selected: None,
            emoji_pictures: RefCell::default(),
            cropping: None,
        }
    }

    /// The window moved to a screen of another scale: fit the screenshot
    /// again, keeping its marks.
    pub(super) fn rescale(&mut self, scale: f32) {
        self.view = view_for(self.view_area, scale);
        self.scale = scale;
    }

    /// The whole screenshot, as a region.
    pub(super) fn whole(&self) -> Region {
        let (width, height) = self.shot.size();
        Region {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// The part of the screenshot to show: its crop, or all of it while
    /// cropping or uncropped.
    fn area(&self) -> Region {
        match self.marks.crop() {
            Some(crop) if self.cropping.is_none() => crop,
            _ => self.whole(),
        }
    }

    /// Fit the view again if the part to show changed: cropped, cropping,
    /// or a crop undone.
    pub(super) fn refit(&mut self) {
        let area = self.area();
        if area != self.view_area {
            self.view = view_for(area, self.scale);
            self.view_area = area;
        }
    }

    /// Done with: GPUI keeps every image it has drawn until told otherwise.
    pub(super) fn release(self, window: &mut Window) {
        let _ = window.drop_image(self.shot.image);
        if let Some(drawn) = self.drawn {
            let _ = window.drop_image(drawn.image);
        }
        if let Some(patch) = self.patch {
            let _ = window.drop_image(patch.image);
        }
        let mut pictures = self.emoji_pictures.into_inner();
        pictures.drop_stale(window);
        for (_, image) in pictures.kept {
            let _ = window.drop_image(image);
        }
    }

    /// Let go of the emoji pictures the last frame stopped showing.
    pub(super) fn drop_stale_pictures(&self, window: &mut Window) {
        self.emoji_pictures.borrow_mut().drop_stale(window);
    }

    fn drop_patch(&mut self, window: &mut Window) {
        if let Some(patch) = self.patch.take() {
            let _ = window.drop_image(patch.image);
        }
    }

    /// The screenshot with its marks so far.
    pub(super) fn marked(&self) -> Shot {
        Shot {
            marks: self.marks.marks().to_vec(),
            crop: self.marks.crop(),
            ..self.shot.clone()
        }
    }

    /// The marks drawn into the screenshot: all but the selected shape.
    fn background(&self) -> Vec<Mark> {
        let marks = self.marks.marks().iter().enumerate();
        marks
            .filter(|(i, _)| Some(*i) != self.selected)
            .map(|(_, mark)| mark.clone())
            .collect()
    }

    /// What to show: an image, and the finished marks not drawn into it
    /// yet. After an undo or redo, the last drawing stays, unchanged, until
    /// the next is ready: painting its marks again meanwhile would flash.
    fn layers(&self) -> (Arc<RenderImage>, Vec<Mark>) {
        let background = self.background();
        match &self.drawn {
            Some(drawn) => {
                let pending = missing(&background, &drawn.marks).unwrap_or_default();
                (drawn.image.clone(), pending)
            }
            None => (self.shot.image.clone(), background),
        }
    }

    /// The screenshot pixel under canvas point `at`; `None` off the
    /// screenshot.
    fn pixel_at(&self, at: Xy, canvas: Xy) -> Option<(f32, f32)> {
        let (x, y) = self.placing(canvas).pixel(at);
        let area = self.view_area;
        let across = area.x as f32..(area.x + area.width) as f32;
        let down = area.y as f32..(area.y + area.height) as f32;
        (across.contains(&x) && down.contains(&y)).then_some((x, y))
    }

    /// The stroke width, screenshot pixels, for a tool's `size`: in the
    /// logical pixels of the screen the screenshot was taken on.
    pub(super) fn width_for(&self, size: f32) -> f32 {
        size * self.shot.scale.unwrap_or(self.scale)
    }

    /// The screenshot pixel under canvas point `at`, on the screenshot or
    /// off it.
    fn pixel_anywhere(&self, at: Xy, canvas: Xy) -> (f32, f32) {
        self.placing(canvas).pixel(at)
    }

    /// Where the screenshot sits in a canvas of size `canvas`.
    pub(super) fn placing(&self, canvas: Xy) -> Placing {
        let placed = self.view.placement(canvas);
        let area = self.view_area;
        let per_pixel = placed.size.x / area.width as f32;
        Placing {
            origin: Xy::new(
                placed.origin.x - area.x as f32 * per_pixel,
                placed.origin.y - area.y as f32 * per_pixel,
            ),
            per_pixel,
            seen_origin: placed.origin,
            seen_size: placed.size,
        }
    }
}

impl Shown {
    /// The marks to paint over the drawing: `pending` ones not drawn into
    /// it yet, the selected shape, and the one being drawn, each emoji with
    /// its picture at the size it shows (`per_pixel` canvas pixels a
    /// screenshot pixel, `scale` device pixels a canvas one). The pen and
    /// the shapes paint over, as the drawing has them; the highlighter
    /// multiplies, which only its patch shows, so until the patch is ready
    /// it is left out rather than shown wrong.
    fn live(
        &self,
        pending: Vec<Mark>,
        per_pixel: f32,
        scale: f32,
    ) -> Vec<(Mark, Option<Arc<RenderImage>>)> {
        let drawing = match &self.gesture {
            Some(Gesture::Draw(drawing)) => Some(Mark::Stroke(drawing.stroke.clone())),
            Some(Gesture::Shape {
                shape, drawn: true, ..
            }) => Some(Mark::Shape(shape.clone())),
            _ => None,
        };
        let live: Vec<Mark> = pending
            .into_iter()
            .chain(self.selection_now().map(Mark::Shape))
            .chain(drawing)
            .filter(paints_over)
            .collect();
        let wanted: Vec<Option<Picture>> = live
            .iter()
            .map(|mark| {
                let shape = mark.as_shape()?;
                let ShapeKind::Emoji(emoji) = shape.kind else {
                    return None;
                };
                let side = (shape.end.0 - shape.start.0).abs() * per_pixel * scale;
                Some(Picture {
                    emoji,
                    side: side.round().max(1.) as u32,
                    angle: (shape.angle * 10.).round() as i32,
                })
            })
            .collect();
        let pictures = self.emoji_pictures.borrow_mut().for_frame(&wanted);
        live.into_iter().zip(pictures).collect()
    }
}

/// The screenshot drawn at `placing`: `image` and any highlighter patch,
/// clipped to the part shown (its crop), with a border just outside.
fn screenshot(shown: &Shown, image: Arc<RenderImage>, placing: Placing) -> impl IntoElement {
    let (seen, per_pixel) = (placing.seen_origin, placing.per_pixel);
    // Where a screenshot pixel sits in the part shown.
    let inset = |x: u32, y: u32| {
        (
            placing.origin.x - seen.x + x as f32 * per_pixel,
            placing.origin.y - seen.y + y as f32 * per_pixel,
        )
    };
    let placed = |image: Arc<RenderImage>, region: Region| {
        let (left, top) = inset(region.x, region.y);
        img(image)
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(region.width as f32 * per_pixel))
            .h(px(region.height as f32 * per_pixel))
    };
    let patch = shown
        .patch
        .as_ref()
        .map(|patch| placed(patch.image.clone(), patch.region));
    div()
        .absolute()
        .left(px(seen.x - 1.))
        .top(px(seen.y - 1.))
        .border_1()
        .border_color(border())
        .child(
            div()
                .relative()
                .w(px(placing.seen_size.x))
                .h(px(placing.seen_size.y))
                .overflow_hidden()
                .child(placed(image, shown.whole()))
                .children(patch),
        )
}

/// What the pointer shows over the canvas: a cursor GPUI has, or one it
/// lacks on Windows, which the window shows itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pointer {
    Style(CursorStyle),
    Hand(cursors::Hand),
    /// The four-way arrow, for moving a shape or the crop frame.
    Move,
}

/// Have the window show `over`'s pointer over the canvas (of its size,
/// under the toolbar), if it is one GPUI lacks; otherwise, or with
/// `None`, leave the pointer to GPUI.
pub(super) fn show_pointer(window: &Window, over: Option<(Pointer, Xy)>) {
    let Some(hwnd) = popup::raw_hwnd(window) else {
        return;
    };
    let scale = window.scale_factor();
    let Some((pointer, canvas)) = over else {
        return shuttercrab_platform::cursor::show(hwnd, None);
    };
    let cursor = match pointer {
        Pointer::Hand(hand) => cursors::hand(hand, scale),
        Pointer::Move => Cursor::move_all().ok(),
        Pointer::Style(_) => None,
    };
    let physical = |logical: f32| (logical * scale).round() as i32;
    let area = (
        0,
        physical(TOOLBAR_HEIGHT),
        physical(canvas.x),
        physical(canvas.y),
    );
    let over = cursor.map(|cursor| CursorOver { cursor, area });
    shuttercrab_platform::cursor::show(hwnd, over);
}

/// The marks of `all` that `drawn` lacks, if `drawn` is some of them,
/// in order; `None` if it has any `all` does not.
fn missing(all: &[Mark], drawn: &[Mark]) -> Option<Vec<Mark>> {
    let mut drawn = drawn.iter().peekable();
    let mut lacking = Vec::new();
    for mark in all {
        if drawn.peek() == Some(&mark) {
            drawn.next();
        } else {
            lacking.push(mark.clone());
        }
    }
    drawn.peek().is_none().then_some(lacking)
}

/// `area` of a screenshot fitted to the canvas, its size in the window's
/// logical pixels at full size.
fn view_for(area: Region, scale: f32) -> ShotView {
    ShotView::new(Xy::new(
        area.width as f32 / scale,
        area.height as f32 / scale,
    ))
}

/// How far one notch of the scroll wheel moves the screenshot, logical
/// pixels.
const WHEEL_LINE: f32 = 40.;

/// The eraser's reach around the pointer, logical pixels on screen: the
/// same whatever the zoom.
const ERASER_RADIUS: f32 = 8.;

/// The canvas's size: the window less the toolbar and the footer.
pub(super) fn canvas_size(window: &Window) -> Xy {
    let viewport = window.viewport_size();
    Xy::new(
        f32::from(viewport.width),
        f32::from(viewport.height) - TOOLBAR_HEIGHT - FOOTER_HEIGHT,
    )
}

/// A circle `size` canvas pixels across around canvas point `at`, a light
/// line in a dark one: the pen's tip and the eraser's reach.
fn round_outline(at: Xy, size: f32) -> impl IntoElement {
    div()
        .absolute()
        .left(px(at.x - size / 2. - 1.))
        .top(px(at.y - size / 2. - 1.))
        .size(px(size + 2.))
        .rounded_full()
        .border_1()
        .border_color(gpui_kit::black().opacity(0.6))
        .child(
            div()
                .size_full()
                .rounded_full()
                .border_1()
                .border_color(gpui_kit::white().opacity(0.9)),
        )
}

/// A short message at the foot of the canvas, such as that every mark was
/// taken off.
fn notice(text: &'static str) -> impl IntoElement {
    div()
        .absolute()
        .bottom(px(16.))
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            div()
                .id("notice")
                .aria_label(text)
                .test_support()
                .px_3()
                .py_1p5()
                .rounded_md()
                .bg(rgb(0x2C2C2C))
                .border_1()
                .border_color(border())
                .text_sm()
                .child(text),
        )
}

/// The highlighter's slanted chisel tip, `size` canvas pixels tall,
/// outlined around canvas point `at`: a light line over a dark one.
fn chisel_outline(at: Xy, size: f32) -> impl IntoElement {
    let corners = markup::chisel(size);
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let to_window = |(x, y): (f32, f32)| {
                point(
                    bounds.origin.x + px(at.x + x),
                    bounds.origin.y + px(at.y + y),
                )
            };
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

/// Where the pointer is in the canvas, if it is over it (and over the
/// window: GPUI redraws the window when the pointer leaves it).
pub(super) fn pointer_in_canvas(window: &Window) -> Option<Xy> {
    if !window.is_window_hovered() {
        return None;
    }
    let at = canvas_point(window.mouse_position());
    let canvas = canvas_size(window);
    ((0. ..canvas.x).contains(&at.x) && (0. ..canvas.y).contains(&at.y)).then_some(at)
}

/// A window position as a point in the canvas, which starts under the
/// toolbar.
fn canvas_point(position: Point<Pixels>) -> Xy {
    Xy::new(
        f32::from(position.x),
        f32::from(position.y) - TOOLBAR_HEIGHT,
    )
}

impl MainWindow {
    /// Change the shown screenshot's zoom or position, with the canvas's
    /// size.
    pub(super) fn zoom(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut ShotView, Xy),
    ) {
        if let Some(shown) = &mut self.shown {
            change(&mut shown.view, canvas_size(window));
            cx.notify();
        }
    }

    /// The scroll wheel over the screenshot: with Ctrl, zoom around the
    /// pointer; otherwise move a screenshot bigger than the window (sideways
    /// with Shift).
    fn on_wheel(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(WHEEL_LINE));
        let (x, y) = (f32::from(delta.x), f32::from(delta.y));
        let at = canvas_point(event.position);
        self.zoom(window, cx, |view, canvas| {
            if event.modifiers.control {
                if y != 0. {
                    view.step(if y > 0. { 1 } else { -1 }, Some(at), canvas);
                }
            } else if event.modifiers.shift && x == 0. {
                view.pan(Xy::new(y, 0.), canvas);
            } else {
                view.pan(Xy::new(x, y), canvas);
            }
        });
    }

    pub(super) fn undo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(shown) = &mut self.shown {
            // The selected shape stays picked up if undo only changes it.
            if shown.marks.undo_edits() != shown.selected {
                shown.selected = None;
            }
        }
        if let Some(shown) = &mut self.shown
            && shown.marks.undo()
        {
            shown.drop_patch(window);
            self.redraw(window, cx);
        }
    }

    pub(super) fn redo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(shown) = &mut self.shown
            && shown.marks.redo_edits() != shown.selected
        {
            shown.selected = None;
        }
        if let Some(shown) = &mut self.shown
            && shown.marks.redo()
        {
            shown.drop_patch(window);
            self.redraw(window, cx);
        }
    }

    pub(super) fn on_key_up(&mut self, event: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "space" {
            self.space_held = false;
            // The pointer changes back at once.
            cx.notify();
        }
    }

    /// Draw the marks onto a copy of the screenshot, off the main thread,
    /// and show it once ready, unless the marks changed meanwhile (a later
    /// drawing is then on its way).
    pub(super) fn redraw(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = &mut self.shown else {
            return;
        };
        cx.notify();
        let (base, marks) = (shown.shot.image.clone(), shown.background());
        if marks.is_empty() {
            if let Some(drawn) = shown.drawn.take() {
                let _ = window.drop_image(drawn.image);
            }
            return;
        }
        let (width, height) = shown.shot.size();
        let (onto, drawing) = (base.clone(), marks.clone());
        cx.spawn_in(window, async move |this, cx| {
            let image = cx
                .background_executor()
                .spawn(async move {
                    let bgra = onto.as_bytes(0).unwrap_or_default();
                    let drawn = markup::draw(bgra, width, height, &drawing, true);
                    pixels::bgra_image(drawn, width, height)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(shown) = &mut this.shown else {
                    return;
                };
                if Arc::ptr_eq(&shown.shot.image, &base) && shown.background() == marks {
                    // The drawing now has the patch's stroke.
                    if shown.patch.as_ref().is_some_and(|p| p.index < marks.len()) {
                        shown.drop_patch(window);
                    }
                    if let Some(old) = shown.drawn.replace(Drawn { image, marks }) {
                        let _ = window.drop_image(old.image);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// A press on the canvas: start a stroke or erasing with the tool in
    /// hand, or start moving the screenshot (with Space or Ctrl held, or no
    /// tool).
    fn on_canvas_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let canvas = canvas_size(window);
        let at = canvas_point(event.position);
        let hand = self
            .hand
            .filter(|_| !(self.space_held || event.modifiers.control));
        let brush = hand
            .and_then(Hand::drawing)
            .map(|tool| (tool, self.brush(tool)));
        // Crop mode: the frame, or moving the screenshot (with Space or
        // Ctrl held, or off the frame).
        if let Some(shown) = &self.shown
            && shown.cropping.is_some()
        {
            let placing = shown.placing(canvas);
            let panning = self.space_held || event.modifiers.control;
            let gesture = (!panning)
                .then(|| self.press_crop(shown, at, placing))
                .flatten()
                .or_else(|| shown.view.can_pan(canvas).then_some(Gesture::Pan(at)));
            if let Some(shown) = &mut self.shown {
                shown.gesture = gesture;
            }
            cx.notify();
            return;
        }
        // The Shapes tool and Select pick up, move and change shapes.
        if matches!(hand, Some(Hand::Shape | Hand::Select))
            && let Some(shown) = &self.shown
        {
            let (pixel, placing) = (shown.pixel_anywhere(at, canvas), shown.placing(canvas));
            if let Some(gesture) = self.press_selection(at, pixel, placing, window, cx) {
                if let Some(shown) = &mut self.shown {
                    shown.gesture = Some(gesture);
                }
                cx.notify();
                return;
            }
        }
        let style = self.shape_style();
        let Some(shown) = &mut self.shown else {
            return;
        };
        let pixel = shown.pixel_at(at, canvas);
        shown.gesture = match (hand, brush) {
            (Some(Hand::Draw(_)), Some((tool, brush))) => pixel.map(|pixel| {
                Gesture::Draw(Drawing::new(Stroke {
                    tool,
                    color: brush.color,
                    width: shown.width_for(brush.size),
                    points: vec![pixel],
                }))
            }),
            (Some(Hand::Erase), _) => pixel.map(|pixel| Gesture::Erase {
                last: pixel,
                erased: false,
            }),
            (Some(Hand::Shape), _) => pixel.map(|pixel| Gesture::Shape {
                shape: Shape {
                    kind: style.kind,
                    start: pixel,
                    end: pixel,
                    outline: style.outline,
                    fill: if style.kind.fills() {
                        style.fill
                    } else {
                        Ink::TRANSPARENT
                    },
                    width: shown.width_for(style.size),
                    angle: 0.,
                },
                from: at,
                drawn: false,
            }),
            _ => shown.view.can_pan(canvas).then_some(Gesture::Pan(at)),
        };
        // A press erases where it lands, without moving.
        if hand == Some(Hand::Erase)
            && let Some(pixel) = pixel
        {
            self.erase_to(pixel, canvas, window, cx);
        }
        cx.notify();
    }

    /// A right-click with the Shapes tool or Select: a shape's menu.
    fn on_canvas_right_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.hand, Some(Hand::Shape | Hand::Select)) {
            return;
        }
        let (canvas, at) = (canvas_size(window), canvas_point(event.position));
        let Some(shown) = &self.shown else {
            return;
        };
        let (pixel, placing) = (shown.pixel_anywhere(at, canvas), shown.placing(canvas));
        self.open_shape_context(at, pixel, placing, window, cx);
    }

    /// While erasing, take off every mark the eraser passes over on its way
    /// to `pixel`, as one change for the whole drag.
    fn erase_to(
        &mut self,
        pixel: (f32, f32),
        canvas: Xy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(shown) = &mut self.shown else {
            return;
        };
        // The eraser's reach on screen, in screenshot pixels.
        let reach = ERASER_RADIUS / shown.placing(canvas).per_pixel;
        let Some(Gesture::Erase { last, erased }) = &mut shown.gesture else {
            return;
        };
        let from = std::mem::replace(last, pixel);
        // Checked every half reach along the way, so a quick drag misses
        // nothing.
        let length = (pixel.0 - from.0).hypot(pixel.1 - from.1);
        let steps = ((length / (reach / 2.)).ceil() as usize).max(1);
        let mut took = false;
        for step in 0..=steps {
            let t = step as f32 / steps as f32;
            let at = (
                from.0 + (pixel.0 - from.0) * t,
                from.1 + (pixel.1 - from.1) * t,
            );
            while let Some(index) = shown
                .marks
                .marks()
                .iter()
                .rposition(|mark| mark.touches(at, reach))
            {
                // The drag's first take starts a change; the rest join it.
                shown.marks.erase(index, *erased);
                *erased = true;
                took = true;
            }
        }
        if took {
            shown.drop_patch(window);
            self.redraw(window, cx);
        }
    }

    /// Erase all mark-ups, as one change undo takes back, and say so.
    pub(super) fn erase_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = &mut self.shown else {
            return;
        };
        if shown.marks.clear() {
            shown.drop_patch(window);
            self.redraw(window, cx);
            self.show_notice("All mark-ups erased", cx);
        }
    }

    fn on_canvas_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let canvas = canvas_size(window);
        if self.hand.is_some() || self.is_cropping() {
            // The tip outline, or the crop frame's cursor, follows the
            // pointer.
            cx.notify();
        }
        let Some(shown) = &mut self.shown else {
            return;
        };
        if shown.gesture.is_none() {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            // Let go outside the window.
            return self.finish_gesture(window, cx);
        }
        let at = canvas_point(event.position);
        let pixel = shown.placing(canvas).pixel(at);
        if let Some(Gesture::Crop(drag)) = &shown.gesture {
            let drag = *drag;
            Self::drag_crop(shown, &drag, pixel);
            cx.notify();
            return;
        }
        match &mut shown.gesture {
            Some(Gesture::Pan(from)) => {
                let by = Xy::new(at.x - from.x, at.y - from.y);
                shown.view.pan(by, canvas);
                shown.gesture = Some(Gesture::Pan(at));
            }
            Some(Gesture::Draw(drawing)) => {
                // Shift draws a straight line.
                drawing.extend_to(pixel, event.modifiers.shift);
            }
            Some(Gesture::Erase { .. }) => self.erase_to(pixel, canvas, window, cx),
            Some(Gesture::Shape { shape, from, drawn }) => {
                // Shift: a square, a circle, or a line at 45°.
                shape.end = if event.modifiers.shift {
                    markup::constrained(shape.kind, shape.start, pixel)
                } else {
                    pixel
                };
                *drawn |= (at.x - from.x).abs() + (at.y - from.y).abs() > markup::LEAST_DRAG;
            }
            Some(Gesture::Edit(editing)) => {
                Self::drag_selection(editing, pixel, at, event.modifiers.shift);
            }
            Some(Gesture::Crop(_)) | None => {}
        }
        self.update_patch(window, cx);
        cx.notify();
    }

    /// Bring the highlighter's patch up to the stroke being drawn: drawn
    /// off the main thread, one at a time, the next as soon as one is done
    /// if the stroke has changed meanwhile.
    fn update_patch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = &mut self.shown else {
            return;
        };
        let Some(Gesture::Draw(Drawing { stroke, .. })) = &shown.gesture else {
            return;
        };
        if stroke.tool != Tool::Highlighter || shown.patching {
            return;
        }
        let (width, height) = shown.shot.size();
        let Some(region) = stroke.region(width, height) else {
            return;
        };
        let (source, pending) = shown.layers();
        // Finished marks not in the drawing yet belong in the patch too.
        let mut marks = pending.to_vec();
        marks.push(Mark::Stroke(stroke.clone()));
        let (base, drawn, index) = (
            shown.shot.image.clone(),
            stroke.clone(),
            shown.marks.marks().len(),
        );
        shown.patching = true;
        cx.spawn_in(window, async move |this, cx| {
            let image = cx
                .background_executor()
                .spawn(async move {
                    let pixels = source.as_bytes(0).unwrap_or_default();
                    let bgra = markup::draw_region(pixels, width, region, &marks, true);
                    pixels::bgra_image(bgra, region.width, region.height)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(shown) = &mut this.shown else {
                    return;
                };
                if !Arc::ptr_eq(&shown.shot.image, &base) {
                    return;
                }
                shown.patching = false;
                let patch = Patch {
                    image,
                    region,
                    index,
                };
                if let Some(old) = shown.patch.replace(patch) {
                    let _ = window.drop_image(old.image);
                }
                cx.notify();
                let changed = matches!(
                    &shown.gesture,
                    Some(Gesture::Draw(now)) if now.stroke != drawn
                );
                if changed {
                    this.update_patch(window, cx);
                }
            });
        })
        .detach();
    }

    /// The drag is over: a finished stroke joins the marks.
    fn finish_gesture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = &mut self.shown else {
            return;
        };
        match shown.gesture.take() {
            Some(Gesture::Draw(drawing)) => {
                shown.marks.add(Mark::Stroke(drawing.stroke));
                self.redraw(window, cx);
            }
            // A click, or a drag too short to see, is not a shape.
            // A new shape stays picked up, to change, as in Snipping Tool.
            Some(Gesture::Shape { shape, drawn, .. }) => {
                if drawn {
                    shown.marks.add(Mark::Shape(shape));
                    let index = shown.marks.marks().len() - 1;
                    self.select(Some(index), window, cx);
                } else {
                    cx.notify();
                }
            }
            Some(Gesture::Edit(editing)) => self.finish_editing(editing, cx),
            Some(Gesture::Pan(_) | Gesture::Erase { .. } | Gesture::Crop(_)) => cx.notify(),
            None => {}
        }
    }

    /// The pointer over the canvas: what a drag would do. An open hand over
    /// a screenshot that can move (with Space held, or no tool), closed
    /// while moving it; the four-way arrow while moving a shape.
    fn pointer(&self, shown: &Shown, canvas_area: Xy) -> Pointer {
        let panning = matches!(shown.gesture, Some(Gesture::Pan(_)));
        if !(panning || self.space_held)
            && let Some(pointer) = self.crop_pointer(shown, canvas_area)
        {
            return pointer;
        }
        match (&shown.gesture, self.hand, self.space_held) {
            (Some(Gesture::Pan(_)), ..) => Pointer::Hand(cursors::Hand::Closed),
            (Some(Gesture::Edit(editing)), ..) if editing.grip == Grip::Body => Pointer::Move,
            (_, Some(Hand::Select), false) => Pointer::Style(CursorStyle::Arrow),
            (_, Some(_), false) => Pointer::Style(CursorStyle::Crosshair),
            _ if shown.view.can_pan(canvas_area) => Pointer::Hand(cursors::Hand::Open),
            _ => Pointer::Style(CursorStyle::Arrow),
        }
    }

    /// The screenshot shown, drawn where its zoom and position put it, with
    /// its marks; dragging draws with the tool in hand, or moves it when it
    /// is bigger than the window.
    pub(super) fn canvas(
        &self,
        shown: &Shown,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let canvas_area = canvas_size(window);
        let pointer = self.pointer(shown, canvas_area);
        let over =
            (self.canvas_hovered && self.pointer.is_some()).then_some((pointer, canvas_area));
        show_pointer(window, over);
        let placing = shown.placing(canvas_area);
        let (image, pending) = shown.layers();
        let (per_pixel, scale) = (placing.per_pixel, window.scale_factor());
        let live = shown.live(pending, per_pixel, scale);
        div()
            .id("canvas")
            .test_support()
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .cursor(match pointer {
                Pointer::Style(style) => style,
                // Shown by the window itself, over the arrow.
                Pointer::Hand(_) | Pointer::Move => CursorStyle::Arrow,
            })
            .on_scroll_wheel(cx.listener(Self::on_wheel))
            // Bars and menus on it block the pointer: not over the canvas.
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.canvas_hovered = *hovered;
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_canvas_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_canvas_right_down))
            .on_mouse_move(cx.listener(Self::on_canvas_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| this.finish_gesture(window, cx)),
            )
            .child(screenshot(shown, image, placing))
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let at =
                            |p: Xy| point(bounds.origin.x + px(p.x), bounds.origin.y + px(p.y));
                        let size = size(px(placing.seen_size.x), px(placing.seen_size.y));
                        let mask = Bounds::new(at(placing.seen_origin), size);
                        paint_marks(window, &live, mask, at(placing.origin), per_pixel, scale);
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(self.selection_overlay(shown, placing))
            .children(self.crop_overlay(shown, placing, cx))
            .children(self.tip(shown, per_pixel))
            .children(self.notice.map(notice))
            .children(self.shapes_bar(cx))
            .children(self.shape_context_menu(cx))
    }

    /// The tool in hand's tip outlined at the pointer, while the pointer is
    /// over the canvas: at the size it draws, round for the pen, the slanted
    /// chisel for the highlighter; the eraser's reach. A light line in a
    /// dark one, to show on any screenshot. The shapes have none, only the
    /// crosshair. Labelled with its size for a moment after [ or ].
    fn tip(&self, shown: &Shown, per_pixel: f32) -> Vec<AnyElement> {
        let (Some(hand), Some(at)) = (self.hand, self.pointer) else {
            return Vec::new();
        };
        if self.space_held || matches!(shown.gesture, Some(Gesture::Pan(_))) {
            return Vec::new();
        }
        let (mut tip, side) = match hand {
            Hand::Erase => {
                return vec![round_outline(at, 2. * ERASER_RADIUS).into_any_element()];
            }
            Hand::Shape | Hand::Select => (Vec::new(), 0.),
            Hand::Draw(tool) => {
                let side = (shown.width_for(self.brush(tool).size) * per_pixel).max(3.);
                let outline = match tool {
                    Tool::Pen => round_outline(at, side).into_any_element(),
                    Tool::Highlighter => chisel_outline(at, side).into_any_element(),
                };
                (vec![outline], side)
            }
        };
        if self.size_note
            && let Some(note) = self.size_label()
        {
            tip.push(
                div()
                    .id("size-note")
                    .aria_label(note.clone())
                    .test_support()
                    .absolute()
                    .left(px(at.x + side / 2. + 10.))
                    .top(px(at.y - 11.))
                    .px_1p5()
                    .py_0p5()
                    .rounded_md()
                    .bg(rgb(0x2C2C2C))
                    .border_1()
                    .border_color(border())
                    .text_xs()
                    .child(note)
                    .into_any_element(),
            );
        }
        tip
    }
}

/// Whether a mark looks the same painted over the screenshot as drawn into
/// it: everything but the highlighter, which multiplies.
fn paints_over(mark: &Mark) -> bool {
    match mark {
        Mark::Stroke(stroke) => stroke.tool == Tool::Pen,
        Mark::Shape(_) => true,
    }
}

/// Paint `marks` over the screenshot drawn in `area` (window pixels),
/// `per_pixel` window pixels a screenshot pixel, clipped to it as Copy and
/// Save clip them. (The highlighter shows through its patch.)
/// Emoji are painted from their `pictures`, drawn for `scale` device
/// pixels a logical one.
fn paint_marks(
    window: &mut Window,
    marks: &[(Mark, Option<Arc<RenderImage>>)],
    mask: Bounds<Pixels>,
    origin: Point<Pixels>,
    per_pixel: f32,
    scale: f32,
) {
    let to_window =
        |(x, y): (f32, f32)| point(origin.x + px(x * per_pixel), origin.y + px(y * per_pixel));
    window.with_content_mask(Some(ContentMask { bounds: mask }), |window| {
        for (mark, picture) in marks {
            if let (Mark::Shape(shape), Some(picture)) = (mark, picture) {
                let span = picture.size(0).width.0 as f32 / scale;
                let middle = to_window(shape.center());
                let at = point(middle.x - px(span / 2.), middle.y - px(span / 2.));
                let bounds = Bounds::new(at, size(px(span), px(span)));
                let _ = window.paint_image(
                    bounds,
                    bounds,
                    Corners::default(),
                    picture.clone(),
                    0,
                    false,
                );
                continue;
            }
            match mark {
                Mark::Stroke(stroke) => paint_pen_stroke(window, stroke, per_pixel, to_window),
                Mark::Shape(shape) => paint_shape(window, shape, per_pixel, to_window),
            }
        }
    });
}

/// Paint one shape's layers, as `markup::draw` paints them: flat ends,
/// sharp corners, each as see-through as its ink.
fn paint_shape(
    window: &mut Window,
    shape: &Shape,
    per_pixel: f32,
    to_window: impl Fn((f32, f32)) -> Point<Pixels>,
) {
    for layer in shape.layers() {
        let (points, closed, builder) = match &layer.figure {
            Figure::Fill(points) => (points, true, PathBuilder::fill()),
            Figure::Stroke {
                points,
                closed,
                width,
            } => {
                let width = (width * per_pixel).max(1.);
                let options = StrokeOptions::default()
                    .with_line_width(width)
                    .with_line_cap(LineCap::Butt)
                    .with_line_join(LineJoin::Miter);
                let builder = PathBuilder::stroke(px(width)).with_style(PathStyle::Stroke(options));
                (points, *closed, builder)
            }
        };
        let Some((&first, rest)) = points.split_first() else {
            continue;
        };
        let mut path = builder;
        path.move_to(to_window(first));
        for &at in rest {
            path.line_to(to_window(at));
        }
        if closed {
            path.close();
        }
        if let Ok(path) = path.build() {
            let mut color = rgb(layer.color.hex());
            color.a = f32::from(layer.alpha) / 255.;
            window.paint_path(path, color);
        }
    }
}

/// Paint one pen stroke, its points placed by `to_window`.
fn paint_pen_stroke(
    window: &mut Window,
    stroke: &Stroke,
    per_pixel: f32,
    to_window: impl Fn((f32, f32)) -> Point<Pixels>,
) {
    let width = (stroke.width * per_pixel).max(1.);
    let options = StrokeOptions::default()
        .with_line_width(width)
        .with_line_cap(LineCap::Round)
        .with_line_join(LineJoin::Round);
    let mut path = PathBuilder::stroke(px(width)).with_style(PathStyle::Stroke(options));
    let points = &stroke.points;
    path.move_to(to_window(points[0]));
    // The same curve as `markup::draw`: through each point to the next
    // midpoint.
    for pair in points.windows(2).skip(1) {
        let ((cx, cy), (nx, ny)) = (pair[0], pair[1]);
        path.curve_to(
            to_window(((cx + nx) / 2., (cy + ny) / 2.)),
            to_window((cx, cy)),
        );
    }
    let (lx, ly) = points[points.len() - 1];
    // A click leaves a dot: a line too short to see, with its caps.
    path.line_to(to_window((lx + 0.01, ly)));
    if let Ok(path) = path.build() {
        window.paint_path(path, rgb(stroke.color.hex()));
    }
}
