//! The post-capture thumbnail (PRD §7.6): a small card in the corner of the
//! screen after a screenshot. Click opens the image; dragging it drops the
//! file into another application; it dismisses itself after a few seconds
//! (the countdown pauses while the pointer is over it).
//!
//! It never takes the keyboard, so Ctrl+V straight after a capture still
//! pastes into the application the user was in.

use gpui_kit::{
    Context, EventEmitter, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement as _, Pixels, Point, Render,
    RenderImage, Role, StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    TestSupportExt as _, Window, assets::IconName, component::Icon, div, img,
    prelude::FluentBuilder as _, px, rgb,
};
use std::{sync::Arc, time::Duration};

/// The largest image the card shows, logical pixels.
pub const MAX_WIDTH: f32 = 240.0;
pub const MAX_HEIGHT: f32 = 150.0;
/// The smallest, so tiny captures still make a card worth hitting.
pub const MIN_SIDE: f32 = 64.0;
/// Space around the image inside the card, logical pixels.
pub const PADDING: f32 = 6.0;
/// A press that moves further than this, logical pixels, is a drag.
const DRAG_DISTANCE: f32 = 4.0;

/// What the user did with the thumbnail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbnailEvent {
    /// Open the image.
    Open,
    /// Start dragging the file out.
    Drag,
    /// Closed, by the user or by the countdown.
    Close,
}

/// The card's image size, logical pixels, for a `width` × `height` capture:
/// scaled down to fit, never up, and at least [`MIN_SIDE`] on each side.
pub fn image_size(width: u32, height: u32) -> (f32, f32) {
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);
    let fit = (MAX_WIDTH / w).min(MAX_HEIGHT / h).min(1.0);
    ((w * fit).max(MIN_SIDE), (h * fit).max(MIN_SIDE))
}

/// A screenshot scaled down for the thumbnail: straight-alpha RGBA8.
pub struct Small {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Tightly packed straight-alpha RGBA, scaled down to at most `max_width` ×
/// `max_height` physical pixels.
pub fn scale_down(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    max_width: u32,
    max_height: u32,
) -> Small {
    let full =
        image::RgbaImage::from_raw(width, height, rgba).expect("the buffer matches its size");
    let fit = (max_width as f32 / width as f32)
        .min(max_height as f32 / height as f32)
        .min(1.0);
    let (w, h) = (
        ((width as f32 * fit).round() as u32).max(1),
        ((height as f32 * fit).round() as u32).max(1),
    );
    let small = if (w, h) == (width, height) {
        full
    } else {
        image::imageops::thumbnail(&full, w, h)
    };
    Small {
        width: w,
        height: h,
        rgba: small.into_raw(),
    }
}

/// A scaled-down screenshot as an image GPUI can draw.
pub fn render_image(small: &Small) -> Arc<RenderImage> {
    let mut bgra = small.rgba.clone();
    // GPUI draws BGRA.
    for px in bgra.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(small.width, small.height, bgra)
        .expect("the buffer matches its size");
    Arc::new(RenderImage::new([image::Frame::new(buffer)]))
}

/// Tells whether the pointer is over the card. GPUI updates an element's
/// hover state only on mouse moves inside the window, so leaving the card
/// goes unnoticed; the app asks Windows instead.
pub type PointerProbe = Box<dyn Fn() -> bool>;

pub struct Thumbnail {
    image: Arc<RenderImage>,
    hovered: bool,
    probe: Option<PointerProbe>,
    /// Seconds left before the card closes itself.
    left: u32,
    /// Where the left button went down, while it may still become a click.
    pressed_at: Option<Point<Pixels>>,
    closed: bool,
}

impl EventEmitter<ThumbnailEvent> for Thumbnail {}

impl Thumbnail {
    /// A card for `image` that closes itself after `seconds` without the
    /// pointer over it.
    pub fn new(image: Arc<RenderImage>, seconds: u32, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let done = this.update(cx, |this, cx| {
                    if let Some(probe) = &this.probe {
                        let over = probe();
                        if over != this.hovered {
                            this.hovered = over;
                            cx.notify();
                        }
                    }
                    if !this.hovered {
                        this.left = this.left.saturating_sub(1);
                    }
                    if this.left == 0 {
                        this.close(cx);
                    }
                    this.closed
                });
                if done.unwrap_or(true) {
                    break;
                }
            }
        })
        .detach();
        Self {
            image,
            hovered: false,
            probe: None,
            left: seconds.max(1),
            pressed_at: None,
            closed: false,
        }
    }

    /// Ask `probe`, once a second, whether the pointer is over the card.
    pub fn with_pointer_probe(mut self, probe: PointerProbe) -> Self {
        self.probe = Some(probe);
        self
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if !std::mem::replace(&mut self.closed, true) {
            cx.emit(ThumbnailEvent::Close);
        }
    }

    fn on_down(&mut self, event: &MouseDownEvent, _: &mut Window, _: &mut Context<Self>) {
        self.pressed_at = Some(event.position);
    }

    fn on_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(start) = self.pressed_at else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.pressed_at = None;
            return;
        }
        let (dx, dy) = (
            f32::from(event.position.x - start.x),
            f32::from(event.position.y - start.y),
        );
        if dx.hypot(dy) > DRAG_DISTANCE {
            self.pressed_at = None;
            cx.emit(ThumbnailEvent::Drag);
        }
    }

    fn on_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.pressed_at.take().is_some() && !self.closed {
            self.closed = true;
            cx.emit(ThumbnailEvent::Open);
        }
    }
}

impl Render for Thumbnail {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hovered = self.hovered;
        div()
            .id("thumbnail")
            .role(Role::Button)
            .aria_label("Screenshot: click to open, drag to share")
            .test_support()
            .size_full()
            .relative()
            .p(px(PADDING))
            .bg(rgb(0x202020))
            .border_1()
            .border_color(rgb(0x3A3A3A))
            .cursor_pointer()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.hovered = *hovered;
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_down))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_up))
            .child(
                img(self.image.clone())
                    .size_full()
                    .object_fit(ObjectFit::Contain),
            )
            .when(hovered, |card| {
                card.child(
                    div()
                        .id("thumbnail-close")
                        .role(Role::Button)
                        .aria_label("Close")
                        .test_support()
                        .absolute()
                        .top(px(4.))
                        .right(px(4.))
                        .size(px(22.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(rgb(0x000000).opacity(0.7))
                        .hover(|s| s.bg(rgb(0x3A3A3A)))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseUpEvent, _, cx| {
                                cx.stop_propagation();
                                this.pressed_at = None;
                                this.close(cx);
                            }),
                        )
                        .child(
                            Icon::new(IconName::X)
                                .size(px(14.))
                                .text_color(gpui_kit::white()),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_fit_the_card_without_growing() {
        // 16:9 full screen: limited by the width.
        assert_eq!(image_size(3840, 2160), (240.0, 135.0));
        // Tall: limited by the height.
        assert_eq!(image_size(300, 900), (64.0, 150.0));
        // Small captures stay their size…
        assert_eq!(image_size(120, 90), (120.0, 90.0));
        // …but never below the minimum.
        assert_eq!(image_size(10, 10), (64.0, 64.0));
    }
}
