//! The selection overlay (PRD §7.2, §7.4): the frozen monitor, full screen,
//! with a drag-to-select rectangle, live physical-pixel dimensions and the
//! rest dimmed. It only reports what the user chose; the app closes it and
//! takes the screenshot.

use crate::selection::{Drag, dimensions, to_logical};
use framecut_capture::PhysicalRect;
use gpui_kit::{
    Bounds, Context, CursorStyle, EventEmitter, FocusHandle, Hsla, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ObjectFit, ParentElement as _, Pixels, Render, RenderImage, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, TestSupportExt as _, Window,
    div, hsla, img, px,
};
use std::sync::Arc;

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
    Cancelled,
}

pub struct SelectionOverlay {
    frame: OverlayFrame,
    drag: Option<Drag>,
    dragging: bool,
    focus: FocusHandle,
}

impl EventEmitter<OverlayEvent> for SelectionOverlay {}

/// Dimming over everything outside the selection.
fn dim() -> Hsla {
    hsla(0.0, 0.0, 0.0, 0.4)
}

impl SelectionOverlay {
    pub fn new(frame: OverlayFrame, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            frame,
            drag: None,
            dragging: false,
            focus,
        }
    }

    /// The current selection in physical pixels, if there is one.
    pub fn selection(&self) -> Option<PhysicalRect> {
        self.drag
            .map(|d| d.physical(self.frame.scale, self.frame.width, self.frame.height))
            .filter(|r| !r.is_empty())
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.drag = None;
        self.dragging = false;
        cx.emit(OverlayEvent::Cancelled);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            self.cancel(cx);
        }
    }

    fn on_left_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.drag = Some(Drag::at(event.position));
        self.dragging = true;
        cx.notify();
    }

    fn on_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.dragging
            && let Some(drag) = &mut self.drag
        {
            drag.end = event.position;
            cx.notify();
        }
    }

    fn on_left_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.dragging {
            return;
        }
        self.dragging = false;
        if let Some(drag) = &mut self.drag {
            drag.end = event.position;
        }
        match self.selection() {
            Some(rect) => cx.emit(OverlayEvent::Selected(rect)),
            // A click without a drag selects nothing; keep waiting.
            None => self.drag = None,
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
}

impl Render for SelectionOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selection = self.selection();
        let hole = selection.map(|r| to_logical(r, self.frame.scale));
        let viewport = window.viewport_size();
        let mut root = div()
            .id("overlay")
            .role(Role::Pane)
            .aria_label("Select an area to capture")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .cursor(CursorStyle::Crosshair)
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
            )
            .children(self.dimming(hole, window));
        if let (Some(rect), Some(b)) = (selection, hole) {
            let label = SharedString::from(dimensions(rect));
            // Below the selection, or above it when it reaches the bottom.
            let below = b.origin.y + b.size.height + px(6.);
            let top = if below + px(26.) > viewport.height {
                (b.origin.y - px(30.)).max(px(0.))
            } else {
                below
            };
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
                .child(
                    div()
                        .id("dimensions")
                        .role(Role::Label)
                        .aria_label(label.clone())
                        .test_support()
                        .absolute()
                        .left(b.origin.x)
                        .top(top)
                        .px_1p5()
                        .py_0p5()
                        .rounded_sm()
                        .bg(hsla(0.0, 0.0, 0.0, 0.75))
                        .text_color(gpui_kit::white())
                        .text_xs()
                        .child(label),
                );
        }
        root
    }
}
