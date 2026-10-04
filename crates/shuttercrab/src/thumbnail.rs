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
    RenderImage, Role, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Task,
    TestSupportExt as _, Window, assets::IconName, component::Icon, div, img,
    prelude::FluentBuilder as _, px, rgb,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

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
/// scaled down to fit [`MAX_WIDTH`] × [`MAX_HEIGHT`] if larger, then each
/// side raised to at least [`MIN_SIDE`].
pub fn image_size(width: u32, height: u32) -> (f32, f32) {
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);
    let fit = (MAX_WIDTH / w).min(MAX_HEIGHT / h).min(1.0);
    ((w * fit).max(MIN_SIDE), (h * fit).max(MIN_SIDE))
}

/// A screenshot scaled down for the thumbnail, or its drag image:
/// straight-alpha RGBA8.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Tightly packed straight-alpha RGBA, scaled down to at most `max_width` ×
/// `max_height` physical pixels. Reads `rgba` in place: only the small
/// picture is allocated.
pub fn scale_down(
    rgba: &[u8],
    width: u32,
    height: u32,
    max_width: u32,
    max_height: u32,
) -> Picture {
    let full = image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(width, height, rgba)
        .expect("the buffer matches its size");
    let fit = (max_width as f32 / width as f32)
        .min(max_height as f32 / height as f32)
        .min(1.0);
    let (w, h) = (
        ((width as f32 * fit).round() as u32).max(1),
        ((height as f32 * fit).round() as u32).max(1),
    );
    let rgba = if (w, h) == (width, height) {
        rgba.to_vec()
    } else {
        image::imageops::thumbnail(&full, w, h).into_raw()
    };
    Picture {
        width: w,
        height: h,
        rgba,
    }
}

/// The drag image's look: a sharp picture with squircle corners (a
/// superellipse, which flows into the straight sides more smoothly than a
/// circular arc), its outline softened over `softness` physical pixels,
/// half inside and half outside (a blurred edge, not an inner shadow), at
/// `opacity` overall.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragLook {
    pub softness: f32,
    pub opacity: f32,
    /// How far along each side the corner curve reaches, physical pixels.
    pub corner: f32,
}

/// The superellipse exponent of the corners: 2 is a circle, 4 the classic
/// squircle.
const SQUIRCLE: f32 = 4.0;

/// The owner's choice (2026-09-28): sharp, 90% opaque, a 20-pixel soft
/// edge, squircle corners.
pub const DRAG_LOOK: DragLook = DragLook {
    softness: 20.0,
    opacity: 0.9,
    corner: 32.0,
};

/// Signed distance from a point to the outline of a `w` × `h` rectangle
/// with squircle corners reaching `corner` pixels: negative inside.
fn outline_distance(px: f32, py: f32, w: f32, h: f32, corner: f32) -> f32 {
    let r = corner.clamp(0.0, w.min(h) / 2.0);
    // Distance beyond the corner curves' centres, per axis.
    let ax = (px - w / 2.0).abs() - (w / 2.0 - r);
    let ay = (py - h / 2.0).abs() - (h / 2.0 - r);
    if r > 0.0 && ax > 0.0 && ay > 0.0 {
        let norm = ((ax / r).powf(SQUIRCLE) + (ay / r).powf(SQUIRCLE)).powf(1.0 / SQUIRCLE);
        r * (norm - 1.0)
    } else {
        ax.max(ay) - r
    }
}

/// `small` with a soft squircle outline, on a canvas grown by the outer
/// half of the softness; edge pixels continue outward as they fade.
pub fn soften(picture: &Picture, look: DragLook) -> Picture {
    let r = look.softness.max(0.0) / 2.0;
    let pad = r.ceil() as u32;
    let (w, h) = (picture.width, picture.height);
    let (width, height) = (w + 2 * pad, h + 2 * pad);
    let opacity = look.opacity.clamp(0.0, 1.0);
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            // Position relative to the picture, pixel centres.
            let px = x as f32 - pad as f32 + 0.5;
            let py = y as f32 - pad as f32 + 0.5;
            let outside = outline_distance(px, py, w as f32, h as f32, look.corner);
            let cover = if r > 0.0 {
                let t = ((r - outside) / (2.0 * r)).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            } else if outside <= 0.0 {
                1.0
            } else {
                0.0
            };
            let sx = (px.floor() as i64).clamp(0, w as i64 - 1) as u32;
            let sy = (py.floor() as i64).clamp(0, h as i64 - 1) as u32;
            let src = &picture.rgba[((sy * w + sx) * 4) as usize..][..4];
            let a = (src[3] as f32 * cover * opacity).round() as u8;
            rgba.extend_from_slice(&[src[0], src[1], src[2], a]);
        }
    }
    Picture {
        width,
        height,
        rgba,
    }
}

/// A scaled-down screenshot as an image GPUI can draw.
pub fn render_image(picture: &Picture) -> Arc<RenderImage> {
    let mut bgra = picture.rgba.clone();
    // GPUI draws BGRA.
    for px in bgra.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(picture.width, picture.height, bgra)
        .expect("the buffer matches its size");
    Arc::new(RenderImage::new([image::Frame::new(buffer)]))
}

pub struct Thumbnail {
    image: Arc<RenderImage>,
    hovered: bool,
    /// The window's hover state as GPUI last reported it. The card fills its
    /// window, so this says whether the pointer is over the card; unlike an
    /// element's hover, it also clears when the pointer leaves the window.
    window_hovered: bool,
    /// Time left before the card closes itself.
    left: Duration,
    /// When the countdown last started running; `None` while it waits.
    running_since: Option<Instant>,
    /// Closes the card when the time is up; dropping it stops the countdown.
    countdown: Option<Task<()>>,
    /// Where the left button went down, while it may still become a click.
    pressed_at: Option<Point<Pixels>>,
    closed: bool,
}

impl EventEmitter<ThumbnailEvent> for Thumbnail {}

impl Thumbnail {
    /// A card for `image` that closes itself after `seconds` without the
    /// pointer over it.
    pub fn new(image: Arc<RenderImage>, seconds: u32, cx: &mut Context<Self>) -> Self {
        let mut card = Self {
            image,
            hovered: false,
            window_hovered: false,
            left: Duration::from_secs(seconds.max(1).into()),
            running_since: None,
            countdown: None,
            pressed_at: None,
            closed: false,
        };
        card.run_countdown(cx);
        card
    }

    /// The pointer moved onto or off the card: the countdown waits while it
    /// is over the card and carries on when it leaves.
    pub fn set_pointer_over(&mut self, over: bool, cx: &mut Context<Self>) {
        if over != self.hovered {
            self.pointer_changed(over, cx);
            cx.notify();
        }
    }

    fn pointer_changed(&mut self, over: bool, cx: &mut Context<Self>) {
        self.hovered = over;
        if over {
            self.stop_countdown(cx);
        } else {
            self.run_countdown(cx);
        }
    }

    fn run_countdown(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        let left = self.left;
        self.running_since = Some(cx.background_executor().now());
        self.countdown = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(left).await;
            this.update(cx, |this, cx| this.close(cx)).ok();
        }));
    }

    fn stop_countdown(&mut self, cx: &mut Context<Self>) {
        if let Some(since) = self.running_since.take() {
            let ran = cx
                .background_executor()
                .now()
                .saturating_duration_since(since);
            self.left = self.left.saturating_sub(ran);
        }
        self.countdown = None;
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.running_since = None;
        self.countdown = None;
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // GPUI redraws the window when the pointer enters or leaves it.
        let over = window.is_window_hovered();
        if over != self.window_hovered {
            self.window_hovered = over;
            if over != self.hovered {
                self.pointer_changed(over, cx);
            }
        }
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
    fn softening_blurs_the_outline_but_keeps_the_picture_sharp() {
        let picture = Picture {
            width: 40,
            height: 20,
            rgba: vec![255; 40 * 20 * 4],
        };
        let soft = soften(
            &picture,
            DragLook {
                softness: 6.0,
                opacity: 0.9,
                corner: 0.0,
            },
        );
        // Grown by half the softness on each side.
        assert_eq!((soft.width, soft.height), (46, 26));
        let alpha = |x: u32, y: u32| soft.rgba[((y * 46 + x) * 4 + 3) as usize];
        // Inside, away from the outline: sharp, at 90%.
        assert_eq!(alpha(23, 13), 230);
        assert_eq!(alpha(10, 13), 230);
        // Either side of the outline, symmetric about half; beyond the
        // softness, nothing.
        let (inner, outer) = (alpha(3, 13) as i32, alpha(2, 13) as i32);
        assert!((130..=155).contains(&inner), "{inner}");
        assert!((inner + outer - 230).abs() <= 2, "{inner} + {outer}");
        assert!(alpha(0, 13) < 25, "{}", alpha(0, 13));
        assert!(alpha(0, 0) <= alpha(0, 13));
        // Colour continues outward unchanged.
        assert_eq!(soft.rgba[(13 * 46) * 4], 255);
    }

    #[test]
    fn squircle_corners_are_fuller_than_circles_and_sides_stay_straight() {
        let (w, h, r) = (200.0, 100.0, 32.0);
        // The middle of a side is on the outline.
        assert!(outline_distance(100.0, 0.0, w, h, r).abs() < 1e-3);
        // The box's own corner is cut off.
        assert!(outline_distance(0.0, 0.0, w, h, r) > 5.0);
        // Where a circular corner would be, at 45°, the squircle is still
        // inside: it hugs the corner more.
        let c = r - r / 2f32.sqrt();
        assert!(outline_distance(c, c, w, h, r) < -1.0);
        // No corner: a plain rectangle.
        assert!(outline_distance(0.0, 0.0, w, h, 0.0).abs() < 1e-3);
    }

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
