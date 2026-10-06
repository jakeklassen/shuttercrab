//! The pointer's hands for moving a screenshot, open and closed, which
//! GPUI cannot show on Windows: Windows has no such cursors, so they are
//! drawn here from the toolbar's icon set (Lucide's hand and hand-grab),
//! white lines over a dark edge to show on any screenshot.

use crate::icons::Icons;
use gpui_kit::{AssetSource as _, assets::IconName};
use shuttercrab_platform::cursor::Cursor;
use std::{cell::RefCell, collections::HashMap};
use tiny_skia::{Pixmap, Transform};

/// A hand, open over a screenshot that can move, closed while moving it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hand {
    Open,
    Closed,
}

/// A cursor's side at 100 % scale, pixels: Windows' standard size.
const SIDE: f32 = 32.;

thread_local! {
    /// Each hand made, by its size; `None` if it could not be.
    static MADE: RefCell<HashMap<(Hand, u32), Option<Cursor>>> = RefCell::default();
}

/// `hand` as a cursor for a screen of `scale`, made the first time.
pub fn hand(hand: Hand, scale: f32) -> Option<Cursor> {
    let side = (SIDE * scale).round() as u32;
    MADE.with(|made| {
        *made.borrow_mut().entry((hand, side)).or_insert_with(|| {
            let cursor = Cursor::from_rgba(&draw(hand, side)?, side, (side / 2, side / 2));
            cursor
                .inspect_err(|e| log::warn!("no {hand:?} hand cursor: {e:#}"))
                .ok()
        })
    })
}

/// `hand` drawn `side` pixels square: straight-alpha RGBA, white lines on
/// a dark edge.
fn draw(hand: Hand, side: u32) -> Option<Vec<u8>> {
    let icon = match hand {
        Hand::Open => IconName::Hand,
        Hand::Closed => IconName::HandGrab,
    };
    let svg = String::from_utf8(Icons.load(&icon.path()).ok()??.into_owned()).ok()?;
    let mut pixmap = Pixmap::new(side, side)?;
    // The icon is 24 units square, its lines a unit from the edge: drawn a
    // little smaller, so the dark edge fits.
    let scale = side as f32 / 27.;
    let place = Transform::from_scale(scale, scale).pre_translate(1.5, 1.5);
    // The dark edge, then the white lines on it.
    for (color, width) in [("black", "4.5"), ("white", "2")] {
        let lines = svg
            .replace("currentColor", color)
            .replace("stroke-width=\"2\"", &format!("stroke-width=\"{width}\""));
        let tree = resvg::usvg::Tree::from_str(&lines, &resvg::usvg::Options::default()).ok()?;
        resvg::render(&tree, place, &mut pixmap.as_mut());
    }
    Some(
        pixmap
            .pixels()
            .iter()
            .flat_map(|px| {
                let px = px.demultiply();
                [px.red(), px.green(), px.blue(), px.alpha()]
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_hands_draw_white_on_dark() {
        for hand in [Hand::Open, Hand::Closed] {
            let rgba = draw(hand, 48).expect("drawn");
            let pixels = rgba.as_chunks::<4>().0;
            let opaque = |px: &&[u8; 4]| px[3] == 255;
            assert!(
                pixels.iter().filter(opaque).any(|px| px[0] == 255),
                "{hand:?}"
            );
            assert!(
                pixels.iter().filter(opaque).any(|px| px[0] == 0),
                "{hand:?}"
            );
            // Clear around it.
            assert_eq!(pixels[0][3], 0);
        }
    }
}
