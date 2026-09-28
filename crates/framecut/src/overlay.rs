//! The selection overlay (PRD §7.2–7.4): the frozen monitor, full screen.
//! In Area mode the user drags a rectangle, with live physical-pixel
//! dimensions, the rest dimmed, and edges snapping to nearby windows. Space
//! switches to Window mode, which highlights the window under the pointer
//! (or the whole display over the desktop) for a click to capture. The
//! overlay only reports what the user chose; the app closes it and takes
//! the screenshot.

use crate::selection::{
    Drag, ScreenWindow, Snapping, Stuck, dimensions, snap, to_logical, window_at,
};
use framecut_capture::PhysicalRect;
use gpui_kit::{
    Bounds, Context, CursorStyle, EventEmitter, FocusHandle, Hsla, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ObjectFit, ParentElement as _, Pixels, Point, Render, RenderImage, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, TestSupportExt as _, Window,
    div, hsla, img, point, px, rgb,
};
use std::sync::Arc;

/// An edge catches the pointer within this many logical pixels…
const SNAP_CATCH: f32 = 10.0;
/// …and holds it until the pointer is this far away.
const SNAP_RELEASE: f32 = 24.0;

/// The frozen monitor as the overlay shows it.
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
}

/// What the user did with the overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayEvent {
    /// A region of the frozen monitor, physical pixels relative to it.
    Selected(PhysicalRect),
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
}

/// How the pointer picks what to capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Area,
    Window,
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
    mode: Mode,
    drag: Option<Drag>,
    /// The edges the drag's start and end are held to.
    start_stuck: Stuck,
    end_stuck: Stuck,
    dragging: bool,
    /// Where the pointer went down in Window mode.
    pressed: bool,
    focus: FocusHandle,
}

impl EventEmitter<OverlayEvent> for SelectionOverlay {}

/// Dimming over everything outside the selection.
fn dim() -> Hsla {
    hsla(0.0, 0.0, 0.0, 0.4)
}

/// Framecut's accent, for the Window-mode highlight.
fn accent() -> Hsla {
    rgb(0x1F6FEB).into()
}

impl SelectionOverlay {
    pub fn new(frame: OverlayFrame, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            frame,
            windows: Vec::new(),
            snap: false,
            mode: Mode::Area,
            drag: None,
            start_stuck: Stuck::default(),
            end_stuck: Stuck::default(),
            dragging: false,
            pressed: false,
            focus,
        }
    }

    /// The windows on this monitor, front to back, for Window mode and,
    /// with `snap`, for snapping Area selections to their edges.
    pub fn with_windows(mut self, windows: Vec<ScreenWindow>, snap: bool) -> Self {
        self.windows = windows;
        self.snap = snap;
        self
    }

    pub fn mode(&self) -> Mode {
        self.mode
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
        if self.dragging {
            return;
        }
        self.mode = match self.mode {
            Mode::Area => Mode::Window,
            Mode::Window => Mode::Area,
        };
        self.drag = None;
        self.pressed = false;
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
        match self.mode {
            Mode::Area => {
                let (start, stuck) = self.snapped(event.position, Stuck::default());
                self.drag = Some(Drag::at(start));
                self.start_stuck = stuck;
                self.end_stuck = Stuck::default();
                self.dragging = true;
            }
            Mode::Window => self.pressed = true,
        }
        cx.notify();
    }

    fn on_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Area => {
                if self.dragging {
                    let (end, stuck) = self.snapped(event.position, self.end_stuck);
                    self.end_stuck = stuck;
                    if let Some(drag) = &mut self.drag {
                        drag.end = end;
                    }
                    cx.notify();
                }
            }
            // The highlight follows the pointer.
            Mode::Window => cx.notify(),
        }
    }

    fn on_left_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Area => {
                if !self.dragging {
                    return;
                }
                self.dragging = false;
                let (end, stuck) = self.snapped(event.position, self.end_stuck);
                self.end_stuck = stuck;
                if let Some(drag) = &mut self.drag {
                    drag.end = end;
                }
                match self.selection() {
                    Some(rect) => cx.emit(OverlayEvent::Selected(rect)),
                    // A click without a drag selects nothing; keep waiting.
                    None => self.drag = None,
                }
            }
            // Capture on release, so the button-up does not reach the
            // window underneath once the overlay is gone.
            Mode::Window => {
                if !std::mem::take(&mut self.pressed) {
                    return;
                }
                let target = self.target_at(event.position);
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

    /// A short hint at the top: what the mouse does and how to switch.
    fn hint(&self, window: &Window) -> impl IntoElement {
        let text: SharedString = match self.mode {
            Mode::Area => "Drag to capture an area  ·  Space: window  ·  Esc: cancel",
            Mode::Window => "Click a window, or the desktop for the whole display  ·  Space: area  ·  Esc: cancel",
        }
        .into();
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
            .cursor(match self.mode {
                Mode::Area => CursorStyle::Crosshair,
                Mode::Window => CursorStyle::PointingHand,
            })
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_left_down))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_left_up))
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
        match self.mode {
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
        root.child(self.hint(window))
    }
}
