//! The Shapes bar's emoji, as Snipping Tool's: Emoji (E) opens its 18 in a
//! 6 × 3 grid; arrows and Enter, or a click, pick one. It lands 64 pixels
//! square in the middle of the view, picked up, to move, resize (keeping
//! it square) and turn; another, while one sits unmoved there, lands a
//! little down and right of it.

use super::{MainWindow, canvas::canvas_size};
use crate::{
    markup::{self, Emoji, Ink, Mark, Shape, ShapeKind},
    palette::{border, coral, hover, muted, tile},
    pixels,
};
use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, MouseButton, MouseUpEvent, ParentElement as _,
    RenderImage, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, assets::IconName, component::Icon, deferred, div, img,
    prelude::FluentBuilder as _, px, rgb,
};
use std::sync::Arc;

/// Emoji in a row, as in Snipping Tool.
const COLUMNS: usize = 6;

/// A new emoji's side, and how far another steps down and right of one
/// unmoved, logical pixels of the screen the screenshot was taken on.
const SIDE: f32 = 64.;
const STEP: f32 = 16.;

/// The picker's art, pixels square: crisp at twice its size on screen.
const ART: f32 = 56.;

impl MainWindow {
    /// The picker's art, drawn the first time it is wanted.
    fn emoji_art(&self) -> &[Arc<RenderImage>] {
        self.emoji_art.get_or_init(|| {
            Emoji::ALL
                .map(|emoji| {
                    let (bgra, span) = markup::emoji_image(emoji, ART, 0.);
                    pixels::bgra_image(bgra, span, span)
                })
                .to_vec()
        })
    }

    /// E opens or closes the emoji; while they are open, arrows move
    /// through them and Enter or Space places one. Returns whether the key
    /// was used.
    pub(super) fn on_emoji_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if key == "e" {
            self.toggle_emoji_menu(cx);
            return true;
        }
        let Some(at) = self.emoji_menu else {
            return false;
        };
        let count = Emoji::ALL.len();
        self.emoji_menu = Some(match key {
            "left" => (at + count - 1) % count,
            "right" => (at + 1) % count,
            "up" => (at + count - COLUMNS) % count,
            "down" => (at + COLUMNS) % count,
            "enter" | "space" => {
                self.place_emoji(Emoji::ALL[at], window, cx);
                return true;
            }
            _ => return false,
        });
        cx.notify();
        true
    }

    fn toggle_emoji_menu(&mut self, cx: &mut Context<Self>) {
        let open = self.emoji_menu.is_some();
        self.close_flyouts();
        if !open {
            self.emoji_menu = Some(0);
        }
        cx.notify();
    }

    /// Put `emoji` in the middle of the view, picked up, and close the
    /// picker.
    fn place_emoji(&mut self, emoji: Emoji, window: &mut Window, cx: &mut Context<Self>) {
        self.emoji_menu = None;
        let canvas = canvas_size(window);
        let Some(shown) = &mut self.shown else {
            return;
        };
        let scale = shown.shot.scale.unwrap_or(shown.scale);
        let (width, height) = shown.shot.size();
        let placed = shown.view.placement(canvas);
        // The middle of what the canvas shows of the screenshot.
        let per_pixel = placed.size.x / width as f32;
        let middle = |origin: f32, length: f32, pixels: u32| {
            let seen_from = (-origin).max(0.);
            let seen_to = (length - origin).min(pixels as f32 * per_pixel);
            ((seen_from + seen_to) / 2. / per_pixel).clamp(0., pixels as f32)
        };
        let mut center = (
            middle(placed.origin.x, canvas.x, width),
            middle(placed.origin.y, canvas.y, height),
        );
        // Below and right of any emoji already there, unmoved.
        let taken = |at: (f32, f32)| {
            shown.marks.marks().iter().any(|mark| {
                mark.as_shape().is_some_and(|shape| {
                    let c = shape.center();
                    matches!(shape.kind, ShapeKind::Emoji(_))
                        && (c.0 - at.0).abs() < 0.5
                        && (c.1 - at.1).abs() < 0.5
                })
            })
        };
        for _ in 0..32 {
            if !taken(center) {
                break;
            }
            center = (center.0 + STEP * scale, center.1 + STEP * scale);
        }
        let half = SIDE * scale / 2.;
        shown.marks.add(Mark::Shape(Shape {
            kind: ShapeKind::Emoji(emoji),
            start: (center.0 - half, center.1 - half),
            end: (center.0 + half, center.1 + half),
            outline: Ink::TRANSPARENT,
            fill: Ink::TRANSPARENT,
            width: 0.,
            angle: 0.,
        }));
        let index = shown.marks.marks().len() - 1;
        // Picked up from before: let it go, so this one is.
        shown.selected = None;
        self.select(Some(index), window, cx);
    }

    /// Emoji: its key and icon, with its picker below when open.
    pub(super) fn emoji_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let open = self.emoji_menu.is_some();
        div()
            .relative()
            .child(
                div()
                    .id("shape-e")
                    .role(Role::Button)
                    .aria_label("Emoji (E)")
                    .test_support()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(40.))
                    .rounded_md()
                    .border_1()
                    .border_color(if open {
                        coral()
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .when(open, |d| d.bg(tile()))
                    .hover(|s| s.bg(hover()))
                    .cursor_pointer()
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, cx| this.toggle_emoji_menu(cx)),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(1.))
                            .right(px(3.))
                            .text_size(px(9.))
                            .text_color(muted())
                            .child("E"),
                    )
                    .child(Icon::new(IconName::FaceSlightlySmiling).size(px(18.))),
            )
            .when_some(self.emoji_menu, |d, highlighted| {
                d.child(deferred(self.emoji_panel(highlighted, cx)).with_priority(1))
            })
    }

    /// The 18 emoji in a grid, the one the arrow keys are on ringed.
    fn emoji_panel(&self, highlighted: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let art = self.emoji_art().to_vec();
        let cells = Emoji::ALL
            .into_iter()
            .zip(art)
            .enumerate()
            .map(|(i, (emoji, art))| {
                div()
                    .id(SharedString::from(format!("emoji-{i}")))
                    .role(Role::Button)
                    .aria_label(emoji.name())
                    .test_support()
                    .size(px(40.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .border_1()
                    .border_color(if i == highlighted {
                        muted()
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .hover(|s| s.bg(hover()))
                    .cursor_pointer()
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseUpEvent, window, cx| {
                            this.place_emoji(emoji, window, cx)
                        }),
                    )
                    .child(img(art).size(px(ART / 2.)))
            });
        div()
            .id("emoji-menu")
            .test_support()
            .absolute()
            .top(px(46.))
            .left(px(0.))
            .w(px(COLUMNS as f32 * 44. + 26.))
            .p(px(12.))
            .flex()
            .flex_col()
            .gap_2()
            .rounded_lg()
            .bg(rgb(0x2C2C2C))
            .border_1()
            .border_color(border())
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().flex().flex_wrap().gap(px(4.)).children(cells))
            .child(
                div()
                    .text_xs()
                    .text_color(muted())
                    .child("Arrows and Enter place one"),
            )
    }
}
