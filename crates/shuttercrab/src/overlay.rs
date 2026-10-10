//! The selection overlay (PRD §7.2–7.4): the frozen monitor, full screen.
//! In Area mode the user drags a rectangle, with live physical-pixel
//! dimensions, the rest dimmed, and edges snapping to nearby windows. Space
//! switches to Window mode, which highlights the window under the pointer
//! (or the whole display over the desktop) for a click to capture. The
//! overlay only reports what the user chose; the app closes it and takes
//! the screenshot.
//!
//! The app opens one overlay on every monitor. They share their mode, so
//! Space on any of them switches them all, and in Window mode only the one
//! with the pointer highlights anything. A drag let go of over another
//! monitor ends on its own monitor, at the point nearest the pointer.

use crate::palette::accent;
use crate::selection::{
    Drag, ScreenWindow, Snapping, Stuck, dimensions, snap, to_logical, window_at,
};
use gpui_kit::{
    Bounds, Context, CursorStyle, DispatchPhase, EventEmitter, FocusHandle, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement as _, Pixels, Point, Render,
    RenderImage, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, TestSupportExt as _, Window, canvas, div, fill, hsla, img, point,
    prelude::FluentBuilder as _, px, size,
};
use shuttercrab_capture::PhysicalRect;
use std::{cell::Cell, rc::Rc, sync::Arc};

/// An edge catches the pointer within this many logical pixels…
const SNAP_CATCH: f32 = 10.0;
/// …and holds it until the pointer is this far away.
const SNAP_RELEASE: f32 = 24.0;

/// The frozen monitor as the overlay shows it. Its pixels are the frozen
/// screen's only copy, and the screenshot is cut from them too.
#[derive(Clone)]
pub struct OverlayFrame {
    /// Physical pixels.
    pub width: u32,
    pub height: u32,
    /// Physical pixels per logical pixel on the monitor the overlay covers.
    pub scale: f32,
    pub image: Arc<RenderImage>,
}

impl OverlayFrame {
    /// Wrap tightly packed BGRA8 pixels, top row first.
    pub fn from_bgra(width: u32, height: u32, scale: f32, bgra: Vec<u8>) -> Self {
        let buffer = image::RgbaImage::from_raw(width, height, bgra)
            .expect("the frame buffer matches its size");
        Self {
            width,
            height,
            scale,
            image: Arc::new(RenderImage::new([image::Frame::new(buffer)])),
        }
    }

    /// The pixels, as given to [`OverlayFrame::from_bgra`].
    pub fn bgra(&self) -> &[u8] {
        self.image.as_bytes(0).unwrap_or_default()
    }
}

/// What the user did with the overlay.
#[derive(Clone, Debug, PartialEq)]
pub enum OverlayEvent {
    /// A region of the frozen monitor, physical pixels relative to it.
    Selected(PhysicalRect),
    /// A freeform outline, physical pixels relative to the monitor.
    Shape(Arc<[(f32, f32)]>),
    /// A window, and the part of it visible on this monitor (physical
    /// pixels relative to the monitor) in case it cannot be captured
    /// directly.
    Window {
        hwnd: isize,
        visible: PhysicalRect,
    },
    /// The whole monitor.
    Display,
    Cancelled,
    /// Not a choice: Space switched the shared mode, so the other monitors'
    /// overlays need redrawing.
    ModeChanged(Mode),
}

/// How the pointer picks what to capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Area,
    Window,
    /// Draw around what to capture.
    Freeform,
}

/// What a click in Window mode would capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Window(ScreenWindow),
    Display,
}

pub struct SelectionOverlay {
    frame: OverlayFrame,
    /// Windows on this monitor, front to back.
    windows: Vec<ScreenWindow>,
    snap: bool,
    /// Shared with the other monitors' overlays, if there are any.
    mode: Rc<Cell<Mode>>,
    /// One of several overlays, one per monitor.
    shared: bool,
    drag: Option<Drag>,
    /// The edges the drag's start and end are held to.
    start_stuck: Stuck,
    end_stuck: Stuck,
    dragging: bool,
    /// Where the pointer went down in Window mode.
    pressed: bool,
    /// The outline being drawn in Freeform mode, logical pixels.
    outline: Vec<Point<Pixels>>,
    /// Choosing an area to record: Area mode only.
    recording: bool,
    focus: FocusHandle,
}

impl EventEmitter<OverlayEvent> for SelectionOverlay {}

/// Dimming over everything outside the selection.
fn dim() -> Hsla {
    hsla(0.0, 0.0, 0.0, 0.4)
}

/// Points along the polyline through `points`, `every` logical pixels
/// apart (and each of `points`), for drawing it as dots.
fn trail(points: &[Point<Pixels>], every: f32) -> Vec<Point<Pixels>> {
    let mut out = Vec::with_capacity(points.len() * 2);
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (dx, dy) = (f32::from(b.x - a.x), f32::from(b.y - a.y));
        let steps = ((dx.hypot(dy) / every).ceil() as usize).max(1);
        for i in 0..steps {
            let t = i as f32 / steps as f32;
            out.push(point(a.x + px(dx * t), a.y + px(dy * t)));
        }
    }
    out.extend(points.last());
    out
}

/// A round dot of `diameter` logical pixels centred on `at`.
fn dot(at: Point<Pixels>, diameter: f32, color: Hsla) -> gpui_kit::PaintQuad {
    let r = px(diameter / 2.);
    fill(
        Bounds::new(point(at.x - r, at.y - r), size(px(diameter), px(diameter))),
        color,
    )
    .corner_radii(r)
}

impl SelectionOverlay {
    pub fn new(frame: OverlayFrame, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            frame,
            windows: Vec::new(),
            snap: false,
            mode: Rc::new(Cell::new(Mode::Area)),
            shared: false,
            drag: None,
            start_stuck: Stuck::default(),
            end_stuck: Stuck::default(),
            dragging: false,
            pressed: false,
            outline: Vec::new(),
            recording: false,
            focus,
        }
    }

    /// Choose what to record rather than to capture: an area, or a window
    /// in Window mode (PRD §7.7). Space does not switch between them: each
    /// is recorded differently, so the choice made before stands.
    pub fn for_recording(mut self) -> Self {
        self.recording = true;
        self
    }

    /// One of several overlays, one per monitor, all switched by `mode`.
    pub fn sharing_mode(mut self, mode: Rc<Cell<Mode>>) -> Self {
        self.mode = mode;
        self.shared = true;
        self
    }

    /// The windows on this monitor, front to back, for Window mode and,
    /// with `snap`, for snapping Area selections to their edges.
    pub fn with_windows(mut self, windows: Vec<ScreenWindow>, snap: bool) -> Self {
        self.windows = windows;
        self.snap = snap;
        self
    }

    /// Start in `mode` rather than Area.
    pub fn with_mode(self, mode: Mode) -> Self {
        self.mode.set(mode);
        self
    }

    pub fn mode(&self) -> Mode {
        self.mode.get()
    }

    /// The current selection in physical pixels, if there is one.
    pub fn selection(&self) -> Option<PhysicalRect> {
        self.drag
            .map(|d| d.physical(self.frame.scale, self.frame.width, self.frame.height))
            .filter(|r| !r.is_empty())
    }

    /// A pointer position held to window and monitor edges, if snapping is
    /// on, and the edges it is held to. `held` is what held it before.
    fn snapped(&self, position: Point<Pixels>, held: Stuck) -> (Point<Pixels>, Stuck) {
        if !self.snap {
            return (position, Stuck::default());
        }
        let scale = self.frame.scale;
        let (x, y) = (f32::from(position.x) * scale, f32::from(position.y) * scale);
        let snapping = Snapping {
            catch: SNAP_CATCH * scale,
            release: SNAP_RELEASE * scale,
        };
        let stuck = snap(
            &self.windows,
            x,
            y,
            held,
            snapping,
            self.frame.width,
            self.frame.height,
        );
        let (x, y) = stuck.apply(x, y);
        (point(px(x / scale), px(y / scale)), stuck)
    }

    /// Which sides of the selection are held to an edge: left, top, right,
    /// bottom.
    fn snapped_sides(&self) -> [bool; 4] {
        let Some(d) = self.drag else {
            return [false; 4];
        };
        let (s, e) = (self.start_stuck, self.end_stuck);
        let (left, right) = if d.start.x <= d.end.x {
            (s.x, e.x)
        } else {
            (e.x, s.x)
        };
        let (top, bottom) = if d.start.y <= d.end.y {
            (s.y, e.y)
        } else {
            (e.y, s.y)
        };
        [left, top, right, bottom].map(|side| side.is_some())
    }

    /// What Window mode would capture at `position` (logical pixels).
    fn target_at(&self, position: Point<Pixels>) -> Target {
        let scale = self.frame.scale;
        let (x, y) = (
            (f32::from(position.x) * scale) as i32,
            (f32::from(position.y) * scale) as i32,
        );
        match window_at(&self.windows, x, y) {
            Some(w) => Target::Window(*w),
            None => Target::Display,
        }
    }

    /// The target's rectangle on this monitor, and its full size.
    fn target_rects(&self, target: Target) -> (PhysicalRect, PhysicalRect) {
        let full = PhysicalRect::new(0, 0, self.frame.width, self.frame.height);
        match target {
            Target::Window(w) => (
                w.bounds.clamp_to(self.frame.width, self.frame.height),
                w.bounds,
            ),
            Target::Display => (full, full),
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.drag = None;
        self.dragging = false;
        cx.emit(OverlayEvent::Cancelled);
    }

    fn toggle_mode(&mut self, cx: &mut Context<Self>) {
        if self.dragging || self.recording {
            return;
        }
        let mode = match self.mode.get() {
            Mode::Area => Mode::Window,
            Mode::Window => Mode::Area,
            // Freeform was chosen on purpose; Space leaves it.
            Mode::Freeform => return,
        };
        self.mode.set(mode);
        self.drag = None;
        self.pressed = false;
        cx.emit(OverlayEvent::ModeChanged(mode));
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => self.cancel(cx),
            "space" => self.toggle_mode(cx),
            _ => {}
        }
    }

    fn on_left_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match self.mode.get() {
            Mode::Area => {
                let (start, stuck) = self.snapped(event.position, Stuck::default());
                self.drag = Some(Drag::at(start));
                self.start_stuck = stuck;
                self.end_stuck = Stuck::default();
                self.dragging = true;
            }
            Mode::Window => self.pressed = true,
            Mode::Freeform => {
                self.outline = vec![event.position];
                self.dragging = true;
            }
        }
        cx.notify();
    }

    /// The outline drawn so far, closed, as physical pixels relative to the
    /// monitor; `None` if it encloses too little to capture.
    fn shape(&self) -> Option<Arc<[(f32, f32)]>> {
        let scale = self.frame.scale;
        let points: Vec<(f32, f32)> = self
            .outline
            .iter()
            .map(|p| (f32::from(p.x) * scale, f32::from(p.y) * scale))
            .collect();
        let (mut lo, mut hi) = ((f32::MAX, f32::MAX), (f32::MIN, f32::MIN));
        for &(x, y) in &points {
            (lo.0, lo.1, hi.0, hi.1) = (lo.0.min(x), lo.1.min(y), hi.0.max(x), hi.1.max(y));
        }
        (points.len() >= 3 && hi.0 - lo.0 >= 2.0 && hi.1 - lo.1 >= 2.0).then(|| points.into())
    }

    fn on_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.moved(event.position, event.pressed_button, cx);
    }

    /// A move off this overlay, over another monitor, while a drag begun
    /// here goes on. The overlay keeps the pointer while its button is down,
    /// so it hears these too: the drag follows at the nearest point on this
    /// monitor, flush with its edge however fast the pointer left.
    fn on_move_out(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let near = self.nearest(event.position);
        if near != event.position {
            self.moved(near, event.pressed_button, cx);
        }
    }

    /// The point on this monitor nearest `position` (logical pixels).
    fn nearest(&self, position: Point<Pixels>) -> Point<Pixels> {
        let scale = self.frame.scale;
        let near = |v: Pixels, side: u32| px(f32::from(v).clamp(0., side as f32 / scale));
        point(
            near(position.x, self.frame.width),
            near(position.y, self.frame.height),
        )
    }

    fn moved(
        &mut self,
        position: Point<Pixels>,
        pressed: Option<MouseButton>,
        cx: &mut Context<Self>,
    ) {
        if (self.dragging || self.pressed) && pressed != Some(MouseButton::Left) {
            // Let go where this overlay did not hear it: end the drag where
            // it last was, rather than have it follow an unpressed pointer.
            return self.let_go(None, cx);
        }
        match self.mode.get() {
            Mode::Area => {
                if self.dragging {
                    let (end, stuck) = self.snapped(position, self.end_stuck);
                    self.end_stuck = stuck;
                    if let Some(drag) = &mut self.drag {
                        drag.end = end;
                    }
                    cx.notify();
                }
            }
            // The highlight follows the pointer.
            Mode::Window => cx.notify(),
            Mode::Freeform => {
                // A point every logical pixel or so is plenty.
                let far = |last: &Point<Pixels>| {
                    let (dx, dy) = (position.x - last.x, position.y - last.y);
                    f32::from(dx).abs() + f32::from(dy).abs() >= 1.0
                };
                if self.dragging && self.outline.last().is_none_or(far) {
                    self.outline.push(position);
                    cx.notify();
                }
            }
        }
    }

    fn on_left_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.let_go(Some(event.position), cx);
    }

    /// Let go off this overlay, over another monitor. The overlay keeps the
    /// pointer while its button is down, so a drag begun here ends here, at
    /// the nearest point on this monitor. A window pressed is let be.
    fn on_left_up_out(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.mode.get() == Mode::Window {
            return self.let_go(None, cx);
        }
        self.let_go(Some(self.nearest(event.position)), cx);
    }

    /// End the drag or press at `at`, or where it last was: capture what it
    /// chose.
    fn let_go(&mut self, at: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        match self.mode.get() {
            Mode::Area => {
                if !std::mem::take(&mut self.dragging) {
                    return;
                }
                if let Some(at) = at {
                    let (end, stuck) = self.snapped(at, self.end_stuck);
                    self.end_stuck = stuck;
                    if let Some(drag) = &mut self.drag {
                        drag.end = end;
                    }
                }
                match self.selection() {
                    Some(rect) => cx.emit(OverlayEvent::Selected(rect)),
                    // A click without a drag selects nothing; keep waiting.
                    None => self.drag = None,
                }
            }
            Mode::Freeform => {
                if !std::mem::take(&mut self.dragging) {
                    return;
                }
                self.outline.extend(at);
                match self.shape() {
                    Some(outline) => cx.emit(OverlayEvent::Shape(outline)),
                    // Too small to capture; keep waiting.
                    None => self.outline.clear(),
                }
            }
            // Capture on release, so the button-up does not reach the
            // window underneath once the overlay is gone.
            Mode::Window => {
                if !std::mem::take(&mut self.pressed) {
                    return;
                }
                // Let go somewhere else: changed their mind.
                let Some(at) = at else {
                    return cx.notify();
                };
                let target = self.target_at(at);
                cx.emit(match target {
                    Target::Window(w) => OverlayEvent::Window {
                        hwnd: w.hwnd,
                        visible: self.target_rects(target).0,
                    },
                    Target::Display => OverlayEvent::Display,
                });
            }
        }
        cx.notify();
    }

    /// Dimming panels around `hole` (logical pixels), or over everything.
    fn dimming(&self, hole: Option<Bounds<Pixels>>, window: &Window) -> Vec<gpui_kit::Div> {
        let viewport = window.viewport_size();
        let panel = |left: Pixels, top: Pixels, width: Pixels, height: Pixels| {
            div()
                .absolute()
                .left(left)
                .top(top)
                .w(width)
                .h(height)
                .bg(dim())
        };
        match hole {
            None => vec![panel(px(0.), px(0.), viewport.width, viewport.height)],
            Some(b) => {
                let (right, bottom) = (b.origin.x + b.size.width, b.origin.y + b.size.height);
                vec![
                    panel(px(0.), px(0.), viewport.width, b.origin.y),
                    panel(px(0.), bottom, viewport.width, viewport.height - bottom),
                    panel(px(0.), b.origin.y, b.origin.x, b.size.height),
                    panel(right, b.origin.y, viewport.width - right, b.size.height),
                ]
            }
        }
    }

    /// The size readout, below `b` or above it when `b` reaches the bottom;
    /// inside `b` when it fills the screen.
    fn label(
        id: &'static str,
        text: SharedString,
        b: Bounds<Pixels>,
        window: &Window,
    ) -> impl IntoElement {
        let viewport = window.viewport_size();
        let below = b.origin.y + b.size.height + px(6.);
        let top = if below + px(26.) <= viewport.height {
            below
        } else if b.origin.y >= px(30.) {
            b.origin.y - px(30.)
        } else {
            b.origin.y + px(6.)
        };
        let left = if b.origin.x + px(6.) >= viewport.width - px(120.) {
            viewport.width - px(120.)
        } else {
            b.origin.x.max(px(6.))
        };
        div()
            .id(id)
            .role(Role::Label)
            .aria_label(text.clone())
            .test_support()
            .absolute()
            .left(left)
            .top(top)
            .px_1p5()
            .py_0p5()
            .rounded_sm()
            .bg(hsla(0.0, 0.0, 0.0, 0.75))
            .text_color(gpui_kit::white())
            .text_xs()
            .child(text)
    }

    /// A 3-pixel accent line on each side of the selection `b` that is held
    /// to an edge, so the user can see snapping take hold.
    fn snap_markers(&self, b: Bounds<Pixels>) -> Vec<impl IntoElement> {
        let thick = px(3.);
        let (x, y, w, h) = (b.origin.x, b.origin.y, b.size.width, b.size.height);
        let sides = [
            ("snapped-left", x - px(1.), y, thick, h),
            ("snapped-top", x, y - px(1.), w, thick),
            ("snapped-right", x + w - px(2.), y, thick, h),
            ("snapped-bottom", x, y + h - px(2.), w, thick),
        ];
        sides
            .into_iter()
            .zip(self.snapped_sides())
            .filter(|(_, held)| *held)
            .map(|((id, left, top, width, height), _)| {
                div()
                    .id(id)
                    .test_support()
                    .absolute()
                    .left(left)
                    .top(top)
                    .w(width)
                    .h(height)
                    .bg(accent())
            })
            .collect()
    }

    /// The outline being drawn: white over a dark edge, so it shows on any
    /// background, with a faint dotted line back to where it started.
    ///
    /// Drawn as round dots a pixel apart, not as a GPUI path: a path makes
    /// the window allocate screen-sized path textures, one of them 4×
    /// multisampled, which on a 4K monitor is over 160 MB for a thin line.
    fn outline_view(outline: &[Point<Pixels>]) -> impl IntoElement + use<> {
        let points = outline.to_vec();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let (Some(first), Some(last)) = (points.first(), points.last()) else {
                    return;
                };
                let line = trail(&points, 1.0);
                for (diameter, color) in
                    [(4.0, hsla(0.0, 0.0, 0.0, 0.35)), (2.0, gpui_kit::white())]
                {
                    for p in &line {
                        window.paint_quad(dot(*p, diameter, color));
                    }
                }
                for p in trail(&[*last, *first], 4.0) {
                    window.paint_quad(dot(p, 1.5, hsla(0.0, 0.0, 1.0, 0.5)));
                }
            },
        )
        .absolute()
        .left_0()
        .top_0()
        .size_full()
    }

    /// Hears moves off this overlay while a drag goes on, for
    /// [`Self::on_move_out`]: an element's own move listener hears only the
    /// moves over it.
    fn moves_out(cx: &Context<Self>) -> impl IntoElement + use<> {
        let this = cx.weak_entity();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = this.update(cx, |this, cx| this.on_move_out(event, cx));
                    }
                });
            },
        )
        .absolute()
        .size_0()
    }

    /// A short hint at the top: what the mouse does and how to switch.
    fn hint(&self, window: &Window) -> impl IntoElement {
        let text: SharedString = match self.mode.get() {
            Mode::Area if self.recording => "Drag to record an area  ·  Esc: cancel",
            Mode::Window if self.recording => {
                "Click a window to record it, or the desktop for the whole display  ·  Esc: cancel"
            }
            Mode::Area => "Drag to capture an area  ·  Space: window  ·  Esc: cancel",
            Mode::Window => "Click a window to capture it, or the desktop for the whole display  ·  Space: area  ·  Esc: cancel",
            Mode::Freeform => "Draw around what to capture  ·  Esc: cancel",
        }
        .into();
        // In Window mode a click on the hint itself captures nothing. (An
        // Area drag may start or end over it.)
        let window_mode = self.mode.get() == Mode::Window;
        let ignore =
            |_: &MouseDownEvent, _: &mut Window, cx: &mut gpui_kit::App| cx.stop_propagation();
        let viewport = window.viewport_size();
        div()
            .id("mode-hint")
            .role(Role::Status)
            .aria_label(text.clone())
            .test_support()
            .absolute()
            .top(px(12.))
            .left(px(0.))
            .w(viewport.width)
            .flex()
            .justify_center()
            .child(
                div()
                    .id("mode-hint-text")
                    .when(window_mode, |d| {
                        d.on_mouse_down(MouseButton::Left, ignore)
                            .cursor(CursorStyle::Arrow)
                    })
                    .test_support()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(hsla(0.0, 0.0, 0.0, 0.75))
                    .text_color(gpui_kit::white())
                    .text_xs()
                    .child(text),
            )
    }
}

impl Render for SelectionOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scale = self.frame.scale;
        let mut root = div()
            .id("overlay")
            .role(Role::Pane)
            .aria_label("Select an area to capture")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .cursor(match self.mode.get() {
                Mode::Area | Mode::Freeform => CursorStyle::Crosshair,
                Mode::Window => CursorStyle::PointingHand,
            })
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_left_down))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_left_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_left_up_out))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _: &MouseDownEvent, _, cx| this.cancel(cx)),
            )
            .child(
                img(self.frame.image.clone())
                    .absolute()
                    .left_0()
                    .top_0()
                    .size_full()
                    .object_fit(ObjectFit::Fill),
            );
        match self.mode.get() {
            Mode::Area => {
                let selection = self.selection();
                let hole = selection.map(|r| to_logical(r, scale));
                root = root.children(self.dimming(hole, window));
                if let (Some(rect), Some(b)) = (selection, hole) {
                    let label = SharedString::from(dimensions(rect));
                    root = root
                        .child(
                            div()
                                .id("selection")
                                .role(Role::Group)
                                .aria_label(label.clone())
                                .test_support()
                                .absolute()
                                .left(b.origin.x)
                                .top(b.origin.y)
                                .w(b.size.width)
                                .h(b.size.height)
                                .border_1()
                                .border_color(gpui_kit::white()),
                        )
                        .children(self.snap_markers(b))
                        .child(Self::label("dimensions", label, b, window));
                }
            }
            Mode::Freeform => {
                root = root
                    .children(self.dimming(None, window))
                    .child(Self::outline_view(&self.outline));
            }
            // With several monitors, the highlight follows the pointer: the
            // others only dim.
            Mode::Window if self.shared && !window.is_window_hovered() => {
                root = root.children(self.dimming(None, window));
            }
            Mode::Window => {
                let target = self.target_at(window.mouse_position());
                let (shown, full) = self.target_rects(target);
                let b = to_logical(shown, scale);
                let what = match target {
                    Target::Window(_) => "Window",
                    Target::Display => "Display",
                };
                let label = SharedString::from(format!("{what}  {}", dimensions(full)));
                root = root
                    .children(self.dimming(Some(b), window))
                    .child(
                        div()
                            .id("window-target")
                            .role(Role::Group)
                            .aria_label(label.clone())
                            .test_support()
                            .absolute()
                            .left(b.origin.x)
                            .top(b.origin.y)
                            .w(b.size.width)
                            .h(b.size.height)
                            .border_2()
                            .border_color(accent()),
                    )
                    .child(Self::label("dimensions", label, b, window));
            }
        }
        if self.dragging {
            root = root.child(Self::moves_out(cx));
        }
        root.child(self.hint(window))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trail_has_a_point_every_step_and_ends_on_the_last_point() {
        let trail = trail(&[point(px(0.), px(0.)), point(px(3.), px(4.))], 1.0);
        // A 5-pixel segment: five steps, then its end.
        assert_eq!(trail.len(), 6);
        assert_eq!(trail[0], point(px(0.), px(0.)));
        assert_eq!(trail[1], point(px(0.6), px(0.8)));
        assert_eq!(trail[5], point(px(3.), px(4.)));
        assert!(super::trail(&[], 1.0).is_empty());
    }
}
