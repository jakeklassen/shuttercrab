//! Snipping Tool's 18 emoji, drawn from Microsoft's Fluent Emoji, the art
//! Snipping Tool's picker shows (MIT licence; see `assets/emoji/LICENSE`).
//!
//! An emoji on a screenshot is a [`super::Shape`] of kind
//! [`super::ShapeKind::Emoji`]: a square box, turned like a rectangle's,
//! that the emoji fills.

use super::demultiply;
use serde::{Deserialize, Serialize};
use tiny_skia::{IntSize, Pixmap, PixmapPaint, Transform};

/// One of the emoji, in the picker's order: Snipping Tool's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Emoji {
    RedHeart,
    Star,
    RedQuestionMark,
    CheckMark,
    CrossMark,
    Fire,
    ThumbsUp,
    ThumbsDown,
    ClappingHands,
    RaisingHands,
    Eyes,
    HundredPoints,
    SlightlySmilingFace,
    SlightlyFrowningFace,
    FaceWithOpenMouth,
    SmilingFaceWithHeartEyes,
    FaceWithTearsOfJoy,
    LoudlyCryingFace,
}

impl Emoji {
    pub const ALL: [Emoji; 18] = [
        Emoji::RedHeart,
        Emoji::Star,
        Emoji::RedQuestionMark,
        Emoji::CheckMark,
        Emoji::CrossMark,
        Emoji::Fire,
        Emoji::ThumbsUp,
        Emoji::ThumbsDown,
        Emoji::ClappingHands,
        Emoji::RaisingHands,
        Emoji::Eyes,
        Emoji::HundredPoints,
        Emoji::SlightlySmilingFace,
        Emoji::SlightlyFrowningFace,
        Emoji::FaceWithOpenMouth,
        Emoji::SmilingFaceWithHeartEyes,
        Emoji::FaceWithTearsOfJoy,
        Emoji::LoudlyCryingFace,
    ];

    /// Its name, as Narrator says it.
    pub fn name(self) -> &'static str {
        match self {
            Emoji::RedHeart => "Red heart",
            Emoji::Star => "Star",
            Emoji::RedQuestionMark => "Red question mark",
            Emoji::CheckMark => "Check mark",
            Emoji::CrossMark => "Cross mark",
            Emoji::Fire => "Fire",
            Emoji::ThumbsUp => "Thumbs up",
            Emoji::ThumbsDown => "Thumbs down",
            Emoji::ClappingHands => "Clapping hands",
            Emoji::RaisingHands => "Raising hands",
            Emoji::Eyes => "Eyes",
            Emoji::HundredPoints => "Hundred points",
            Emoji::SlightlySmilingFace => "Slightly smiling face",
            Emoji::SlightlyFrowningFace => "Slightly frowning face",
            Emoji::FaceWithOpenMouth => "Face with open mouth",
            Emoji::SmilingFaceWithHeartEyes => "Smiling face with heart-eyes",
            Emoji::FaceWithTearsOfJoy => "Face with tears of joy",
            Emoji::LoudlyCryingFace => "Loudly crying face",
        }
    }

    fn svg(self) -> &'static [u8] {
        macro_rules! art {
            ($file:literal) => {
                include_bytes!(concat!("../../assets/emoji/", $file, ".svg"))
            };
        }
        match self {
            Emoji::RedHeart => art!("red_heart"),
            Emoji::Star => art!("star"),
            Emoji::RedQuestionMark => art!("red_question_mark"),
            Emoji::CheckMark => art!("check_mark"),
            Emoji::CrossMark => art!("cross_mark"),
            Emoji::Fire => art!("fire"),
            Emoji::ThumbsUp => art!("thumbs_up"),
            Emoji::ThumbsDown => art!("thumbs_down"),
            Emoji::ClappingHands => art!("clapping_hands"),
            Emoji::RaisingHands => art!("raising_hands"),
            Emoji::Eyes => art!("eyes"),
            Emoji::HundredPoints => art!("hundred_points"),
            Emoji::SlightlySmilingFace => art!("slightly_smiling_face"),
            Emoji::SlightlyFrowningFace => art!("slightly_frowning_face"),
            Emoji::FaceWithOpenMouth => art!("face_with_open_mouth"),
            Emoji::SmilingFaceWithHeartEyes => art!("smiling_face_with_heart_eyes"),
            Emoji::FaceWithTearsOfJoy => art!("face_with_tears_of_joy"),
            Emoji::LoudlyCryingFace => art!("loudly_crying_face"),
        }
    }

    fn tree(self) -> resvg::usvg::Tree {
        resvg::usvg::Tree::from_data(self.svg(), &resvg::usvg::Options::default())
            .expect("the emoji art is valid SVG")
    }
}

/// How an emoji `side` pixels square, turned `angle` degrees clockwise
/// about its middle, maps from its art onto a square around (0, 0).
fn placement(tree: &resvg::usvg::Tree, side: f32, angle: f32) -> Transform {
    let size = tree.size();
    Transform::from_rotate(angle)
        .pre_translate(-side / 2., -side / 2.)
        .pre_scale(side / size.width(), side / size.height())
}

/// How far a square `side` pixels across, turned `angle` degrees, reaches
/// from its middle each way.
fn reach(side: f32, angle: f32) -> f32 {
    let (sin, cos) = angle.to_radians().sin_cos();
    // Rounded, so a quarter turn's cosine of nearly nought adds nothing.
    let spread = ((sin.abs() + cos.abs()) * 1e4).round() / 1e4;
    side * spread / 2.
}

/// Draw `emoji` onto `canvas`, `side` pixels square around `center`,
/// turned `angle`, moved by `shift`. `bgr`: the canvas's first byte is
/// blue, as GPUI draws.
pub(super) fn paint(
    canvas: &mut Pixmap,
    emoji: Emoji,
    (center, side, angle): ((f32, f32), f32, f32),
    bgr: bool,
    shift: Transform,
) {
    let reach = reach(side, angle).ceil();
    let (x, y) = (
        (center.0 + shift.tx - reach).floor(),
        (center.1 + shift.ty - reach).floor(),
    );
    let span = (2. * reach) as u32 + 2;
    let Some(mut art) = Pixmap::new(span, span) else {
        return;
    };
    let tree = emoji.tree();
    let at = Transform::from_translate(center.0 + shift.tx - x, center.1 + shift.ty - y);
    resvg::render(
        &tree,
        at.pre_concat(placement(&tree, side, angle)),
        &mut art.as_mut(),
    );
    if bgr {
        for px in art.data_mut().as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
    }
    canvas.draw_pixmap(
        x as i32,
        y as i32,
        art.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
}

/// `emoji` alone, `side` pixels square turned `angle` degrees, in the
/// smallest square that holds it: straight-alpha BGRA, as GPUI draws, with
/// that square's side.
pub fn image(emoji: Emoji, side: f32, angle: f32) -> (Vec<u8>, u32) {
    let span = (2. * reach(side, angle)).ceil().max(1.) as u32;
    let size = IntSize::from_wh(span, span).expect("an emoji has a size");
    let mut art = Pixmap::new(size.width(), size.height()).expect("an emoji has a size");
    let tree = emoji.tree();
    let at = Transform::from_translate(span as f32 / 2., span as f32 / 2.);
    resvg::render(
        &tree,
        at.pre_concat(placement(&tree, side, angle)),
        &mut art.as_mut(),
    );
    let mut bgra = art.take();
    demultiply(&mut bgra);
    for px in bgra.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    (bgra, span)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_emoji_draws() {
        for emoji in Emoji::ALL {
            let (bgra, span) = image(emoji, 32., 0.);
            assert_eq!(span, 32);
            assert!(
                bgra.as_chunks::<4>().0.iter().any(|px| px[3] > 0),
                "{emoji:?} is blank"
            );
        }
    }

    #[test]
    fn a_turned_emoji_needs_a_bigger_square() {
        let (_, span) = image(Emoji::Star, 100., 45.);
        assert_eq!(span, 142);
        assert_eq!(image(Emoji::Star, 100., 90.).1, 100);
    }
}
