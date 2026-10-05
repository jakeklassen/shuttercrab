//! The screenshot in the main window: where it sits (zoom and position),
//! moving it about, and drawing on it with the pen and highlighter.
//!
//! Finished strokes are drawn onto a copy of the screenshot off the main
//! thread ([`markup::draw`]), the same way Copy and Save draw them, so what
//! the window shows is what they give. Until that copy is ready, and while
//! a stroke is being drawn, pen strokes are painted over it directly: they
//! cover what is under them, so that looks the same. GPUI cannot multiply
//! colours, so a highlighter stroke is drawn onto just the patch of the
//! screenshot it covers, as the stroke grows, and shown over it.

use super::{FOOTER_HEIGHT, MainWindow, Shot, TOOLBAR_HEIGHT};
use crate::{
    markup::{self, Brush, Drawing, Marks, Region, Stroke, Tool},
    palette::border,
    pixels,
    shot_view::{ShotView, Xy},
};
use gpui_kit::{
    AnyElement, Bounds, ContentMask, Context, CursorStyle, InteractiveElement as _, IntoElement,
    KeyUpEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    PathBuilder, PathStyle, Pixels, Point, RenderImage, ScrollWheelEvent, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, canvas, div, img,
    point, prelude::FluentBuilder as _, px, rgb, size,
};
use lyon_tessellation::{LineCap, LineJoin, StrokeOptions};
use std::sync::Arc;

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
    gesture: Option<Gesture>,
}

/// The screenshot with `strokes` drawn on it.
struct Drawn {
    image: Arc<RenderImage>,
    strokes: Vec<Stroke>,
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
enum Gesture {
    /// Moving the screenshot; where the pointer last was, in the canvas.
    Pan(Xy),
    /// Drawing a stroke.
    Draw(Drawing),
}

impl Shown {
    pub(super) fn new(shot: Shot, scale: f32) -> Self {
        Self {
            view: view_for(&shot, scale),
            shot,
            scale,
            marks: Marks::default(),
            drawn: None,
            patch: None,
            patching: false,
            gesture: None,
        }
    }

    /// The window moved to a screen of another scale: fit the screenshot
    /// again, keeping its marks.
    pub(super) fn rescale(&mut self, scale: f32) {
        self.view = view_for(&self.shot, scale);
        self.scale = scale;
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
    }

    fn drop_patch(&mut self, window: &mut Window) {
        if let Some(patch) = self.patch.take() {
            let _ = window.drop_image(patch.image);
        }
    }

    /// The screenshot with its marks so far.
    pub(super) fn marked(&self) -> Shot {
        Shot {
            marks: self.marks.strokes().to_vec(),
            ..self.shot.clone()
        }
    }

    /// What to show: an image, and the finished strokes not drawn into it
    /// yet. After an undo or redo, the last drawing stays, unchanged, until
    /// the next is ready: painting its strokes again meanwhile would flash.
    fn layers(&self) -> (Arc<RenderImage>, &[Stroke]) {
        let strokes = self.marks.strokes();
        match &self.drawn {
            Some(drawn) if strokes.starts_with(&drawn.strokes) => {
                (drawn.image.clone(), &strokes[drawn.strokes.len()..])
            }
            Some(drawn) => (drawn.image.clone(), &[]),
            None => (self.shot.image.clone(), strokes),
        }
    }

    /// The screenshot pixel under canvas point `at`; `None` off the
    /// screenshot.
    fn pixel_at(&self, at: Xy, canvas: Xy) -> Option<(f32, f32)> {
        let placed = self.view.placement(canvas);
        let (width, height) = self.shot.size();
        let x = (at.x - placed.origin.x) / placed.size.x * width as f32;
        let y = (at.y - placed.origin.y) / placed.size.y * height as f32;
        ((0. ..width as f32).contains(&x) && (0. ..height as f32).contains(&y)).then_some((x, y))
    }

    /// The stroke width, screenshot pixels, for `brush`: its size in the
    /// logical pixels of the screen the screenshot was taken on.
    fn width_for(&self, brush: Brush) -> f32 {
        brush.size * self.shot.scale.unwrap_or(self.scale)
    }
}

/// A screenshot fitted to the canvas, its size in the window's logical
/// pixels at full size.
fn view_for(shot: &Shot, scale: f32) -> ShotView {
    let (width, height) = shot.size();
    ShotView::new(Xy::new(width as f32 / scale, height as f32 / scale))
}

/// How far one notch of the scroll wheel moves the screenshot, logical
/// pixels.
const WHEEL_LINE: f32 = 40.;

/// The canvas's size: the window less the toolbar and the footer.
pub(super) fn canvas_size(window: &Window) -> Xy {
    let viewport = window.viewport_size();
    Xy::new(
        f32::from(viewport.width),
        f32::from(viewport.height) - TOOLBAR_HEIGHT - FOOTER_HEIGHT,
    )
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
        if let Some(shown) = &mut self.shown
            && shown.marks.undo()
        {
            shown.drop_patch(window);
            self.redraw(window, cx);
        }
    }

    pub(super) fn redo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(shown) = &mut self.shown
            && shown.marks.redo()
        {
            shown.drop_patch(window);
            self.redraw(window, cx);
        }
    }

    pub(super) fn on_key_up(&mut self, event: &KeyUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if event.keystroke.key == "space" {
            self.space_held = false;
        }
    }

    /// Draw the marks onto a copy of the screenshot, off the main thread,
    /// and show it once ready, unless the marks changed meanwhile (a later
    /// drawing is then on its way).
    fn redraw(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = &mut self.shown else {
            return;
        };
        cx.notify();
        if shown.marks.is_empty() {
            if let Some(drawn) = shown.drawn.take() {
                let _ = window.drop_image(drawn.image);
            }
            return;
        }
        let (base, strokes) = (shown.shot.image.clone(), shown.marks.strokes().to_vec());
        let (width, height) = shown.shot.size();
        let (onto, drawing) = (base.clone(), strokes.clone());
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
                if Arc::ptr_eq(&shown.shot.image, &base) && shown.marks.strokes() == strokes {
                    // The drawing now has the patch's stroke.
                    if shown
                        .patch
                        .as_ref()
                        .is_some_and(|p| p.index < strokes.len())
                    {
                        shown.drop_patch(window);
                    }
                    if let Some(old) = shown.drawn.replace(Drawn { image, strokes }) {
                        let _ = window.drop_image(old.image);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// A press on the canvas: start a stroke with the tool in hand, or
    /// start moving the screenshot (with Space or Ctrl held, or no tool).
    fn on_canvas_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let canvas = canvas_size(window);
        let at = canvas_point(event.position);
        let panning = self.space_held || event.modifiers.control;
        let brush = self.tool.map(|tool| (tool, self.brush(tool)));
        let Some(shown) = &mut self.shown else {
            return;
        };
        shown.gesture = match (brush, panning) {
            (Some((tool, brush)), false) => shown.pixel_at(at, canvas).map(|pixel| {
                Gesture::Draw(Drawing::new(Stroke {
                    tool,
                    color: brush.color,
                    width: shown.width_for(brush),
                    points: vec![pixel],
                }))
            }),
            _ => shown.view.can_pan(canvas).then_some(Gesture::Pan(at)),
        };
        cx.notify();
    }

    fn on_canvas_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let canvas = canvas_size(window);
        if self.tool.is_some() {
            // The tip outline follows the pointer.
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
        let placed = shown.view.placement(canvas);
        let (width, height) = shown.shot.size();
        match &mut shown.gesture {
            Some(Gesture::Pan(from)) => {
                let by = Xy::new(at.x - from.x, at.y - from.y);
                shown.view.pan(by, canvas);
                shown.gesture = Some(Gesture::Pan(at));
            }
            Some(Gesture::Draw(drawing)) => {
                let x = (at.x - placed.origin.x) / placed.size.x * width as f32;
                let y = (at.y - placed.origin.y) / placed.size.y * height as f32;
                // Shift draws a straight line.
                drawing.extend_to((x, y), event.modifiers.shift);
            }
            None => {}
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
        // Finished strokes not in the drawing yet belong in the patch too.
        let mut strokes = pending.to_vec();
        strokes.push(stroke.clone());
        let (base, drawn, index) = (
            shown.shot.image.clone(),
            stroke.clone(),
            shown.marks.strokes().len(),
        );
        shown.patching = true;
        cx.spawn_in(window, async move |this, cx| {
            let image = cx
                .background_executor()
                .spawn(async move {
                    let pixels = source.as_bytes(0).unwrap_or_default();
                    let bgra = markup::draw_region(pixels, width, region, &strokes, true);
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
                shown.marks.add(drawing.stroke);
                self.redraw(window, cx);
            }
            Some(Gesture::Pan(_)) => cx.notify(),
            None => {}
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
        let placed = shown.view.placement(canvas_area);
        let (image, pending) = shown.layers();
        // The pen paints over, so painting it directly matches the drawing.
        // The highlighter multiplies, which only its patch shows; until the
        // patch is ready it is left out rather than shown wrong.
        let drawing = match &shown.gesture {
            Some(Gesture::Draw(drawing)) => Some(&drawing.stroke),
            _ => None,
        };
        let live: Vec<Stroke> = pending
            .iter()
            .chain(drawing)
            .filter(|stroke| stroke.tool == Tool::Pen)
            .cloned()
            .collect();
        // Screen pixels per screenshot pixel.
        let per_pixel = placed.size.x / shown.shot.size().0 as f32;
        let patch = shown.patch.as_ref().map(|patch| {
            let r = patch.region;
            img(patch.image.clone())
                .absolute()
                .left(px(placed.origin.x + r.x as f32 * per_pixel))
                .top(px(placed.origin.y + r.y as f32 * per_pixel))
                .w(px(r.width as f32 * per_pixel))
                .h(px(r.height as f32 * per_pixel))
        });
        let cursor = match (&shown.gesture, self.tool, self.space_held) {
            (Some(Gesture::Pan(_)), ..) => CursorStyle::ClosedHand,
            (_, Some(_), false) => CursorStyle::Crosshair,
            _ if shown.view.can_pan(canvas_area) => CursorStyle::OpenHand,
            _ => CursorStyle::Arrow,
        };
        div()
            .id("canvas")
            .test_support()
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .cursor(cursor)
            .on_scroll_wheel(cx.listener(Self::on_wheel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_canvas_down))
            .on_mouse_move(cx.listener(Self::on_canvas_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| this.finish_gesture(window, cx)),
            )
            .child(
                // The border sits just outside the screenshot.
                div()
                    .absolute()
                    .left(px(placed.origin.x - 1.))
                    .top(px(placed.origin.y - 1.))
                    .border_1()
                    .border_color(border())
                    .child(img(image).w(px(placed.size.x)).h(px(placed.size.y))),
            )
            .children(patch)
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let origin = point(
                            bounds.origin.x + px(placed.origin.x),
                            bounds.origin.y + px(placed.origin.y),
                        );
                        let area = Bounds::new(origin, size(px(placed.size.x), px(placed.size.y)));
                        paint_pen_strokes(window, &live, area, per_pixel);
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(self.tip(shown, per_pixel))
    }

    /// The tool in hand's tip outlined at the pointer, while the pointer is
    /// over the canvas, at the size it draws: round for the pen, square for
    /// the highlighter, a light line in a dark one to show on any
    /// screenshot. Labelled with its size for a moment after [ or ].
    fn tip(&self, shown: &Shown, per_pixel: f32) -> Vec<AnyElement> {
        let (Some(tool), Some(at)) = (self.tool, self.pointer) else {
            return Vec::new();
        };
        if self.space_held || matches!(shown.gesture, Some(Gesture::Pan(_))) {
            return Vec::new();
        }
        let brush = self.brush(tool);
        let side = (shown.width_for(brush) * per_pixel).max(3.);
        let round = tool == Tool::Pen;
        let outline = div()
            .absolute()
            .left(px(at.x - side / 2. - 1.))
            .top(px(at.y - side / 2. - 1.))
            .size(px(side + 2.))
            .border_1()
            .border_color(gpui_kit::black().opacity(0.6))
            .when(round, |d| d.rounded_full())
            .child(
                div()
                    .size_full()
                    .border_1()
                    .border_color(gpui_kit::white().opacity(0.9))
                    .when(round, |d| d.rounded_full()),
            )
            .into_any_element();
        let mut tip = vec![outline];
        if self.size_note {
            let name = match tool {
                Tool::Pen => "Pen",
                Tool::Highlighter => "Highlighter",
            };
            let note = SharedString::from(format!("{name} {}", brush.size));
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

/// Paint pen `strokes` over the screenshot drawn in `area` (window
/// pixels), `per_pixel` window pixels a screenshot pixel, clipped to it as
/// Copy and Save clip them. (The highlighter shows through its patch.)
fn paint_pen_strokes(
    window: &mut Window,
    strokes: &[Stroke],
    area: Bounds<Pixels>,
    per_pixel: f32,
) {
    let to_window = |(x, y): (f32, f32)| {
        point(
            area.origin.x + px(x * per_pixel),
            area.origin.y + px(y * per_pixel),
        )
    };
    window.with_content_mask(Some(ContentMask { bounds: area }), |window| {
        for stroke in strokes {
            let width = (stroke.width * per_pixel).max(1.);
            let options = StrokeOptions::default()
                .with_line_width(width)
                .with_line_cap(LineCap::Round)
                .with_line_join(LineJoin::Round);
            let mut path = PathBuilder::stroke(px(width)).with_style(PathStyle::Stroke(options));
            let points = &stroke.points;
            path.move_to(to_window(points[0]));
            // The same curve as `markup::draw`: through each point to the
            // next midpoint.
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
    });
}
