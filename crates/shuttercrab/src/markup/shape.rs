//! Snipping Tool's shapes: rectangle, oval, line and arrow. Each has an
//! outline, and a rectangle or oval a fill too; either can be Transparent,
//! or partly see-through.
//!
//! A shape is kept as it was dragged, from `start` to `end`, and turned into
//! [`Layer`]s, polygons filled or stroked, which both the drawing
//! ([`super::draw`]) and the window's live view paint, so the two agree.

use super::{PEN_COLORS, Rgb, distance};
use serde::{Deserialize, Serialize};
use std::f32::consts::{FRAC_PI_4, PI};

/// A point, screenshot pixels.
type Point = (f32, f32);

/// Which shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShapeKind {
    #[default]
    Rectangle,
    Oval,
    Line,
    Arrow,
}

impl ShapeKind {
    /// In the Shapes bar's order.
    pub const ALL: [ShapeKind; 4] = [
        ShapeKind::Rectangle,
        ShapeKind::Oval,
        ShapeKind::Line,
        ShapeKind::Arrow,
    ];

    /// Whether it has an inside to fill.
    pub fn fills(self) -> bool {
        matches!(self, ShapeKind::Rectangle | ShapeKind::Oval)
    }
}

/// A colour and how opaque it is, or Transparent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ink {
    /// `None` is Transparent: nothing is painted.
    pub color: Option<Rgb>,
    /// Percent, 1 to 100; kept while Transparent, for the next colour.
    pub opacity: u8,
}

impl Ink {
    pub const TRANSPARENT: Ink = Ink {
        color: None,
        opacity: 100,
    };

    /// The colour and its alpha, 0 to 255, unless Transparent. Snipping
    /// Tool's: the percentage of 255, rounded down.
    pub fn paint(self) -> Option<(Rgb, u8)> {
        let alpha = u16::from(self.opacity.min(100)) * 255 / 100;
        self.color.map(|color| (color, alpha as u8))
    }
}

/// The colours a shape's fill and outline offer, as Snipping Tool's:
/// Transparent, then the pen's colours less Iron gray, 30 in its 6 × 5
/// grid.
pub const SHAPE_COLORS: [Option<Rgb>; 30] = {
    let mut colors = [None; 30];
    let mut i = 0;
    let mut at = 1;
    while i < PEN_COLORS.len() {
        // Iron gray, #58595B.
        if i != 5 {
            colors[at] = Some(PEN_COLORS[i]);
            at += 1;
        }
        i += 1;
    }
    colors
};

/// What the next shape is drawn with. The size is the outline's width in
/// logical pixels of the screen the screenshot was taken on, as a pen's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeStyle {
    pub kind: ShapeKind,
    pub outline: Ink,
    pub fill: Ink,
    pub size: f32,
}

impl ShapeStyle {
    /// Snipping Tool's: a red rectangle outline of 4, without a fill.
    pub const DEFAULT: ShapeStyle = ShapeStyle {
        kind: ShapeKind::Rectangle,
        outline: Ink {
            color: Some(Rgb(0xE6, 0x1B, 0x1B)),
            opacity: 100,
        },
        fill: Ink::TRANSPARENT,
        size: 4.,
    };

    /// The outline's sizes, as Snipping Tool's.
    pub const SIZES: std::ops::RangeInclusive<f32> = 1. ..=24.;

    /// The opacities, percent.
    pub const OPACITIES: std::ops::RangeInclusive<u8> = 1..=100;
}

/// A shape on a screenshot, in screenshot pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub kind: ShapeKind,
    /// Where the drag started and ended: opposite corners of a rectangle
    /// or of an oval's box, a line's ends, an arrow's tail and tip.
    pub start: Point,
    pub end: Point,
    pub outline: Ink,
    /// Painted inside a rectangle or an oval; the rest have none.
    pub fill: Ink,
    /// The outline's width.
    pub width: f32,
}

/// One part of a shape to paint, in paint order.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub figure: Figure,
    pub color: Rgb,
    /// 0 to 255.
    pub alpha: u8,
}

/// A polygon to fill, or a line through points to stroke.
#[derive(Clone, Debug, PartialEq)]
pub enum Figure {
    Fill(Vec<Point>),
    /// With flat ends and sharp corners; `closed` joins the last point to
    /// the first.
    Stroke {
        points: Vec<Point>,
        closed: bool,
        width: f32,
    },
}

/// A drag shorter than this, logical pixels on screen, draws no shape: a
/// click is not a shape. Snipping Tool's.
pub const LEAST_DRAG: f32 = 4.;

/// How long an arrow's head is, and how wide, in outline widths.
const ARROW_HEAD: f32 = 5.;

impl Shape {
    /// The parts to paint: a rectangle's or an oval's fill, then its
    /// outline; a line; an arrow, one solid shape in the outline's colour.
    /// Transparent parts are left out.
    pub fn layers(&self) -> Vec<Layer> {
        let layer = |figure, (color, alpha)| Layer {
            figure,
            color,
            alpha,
        };
        let outline = self.outline.paint();
        match self.kind {
            ShapeKind::Rectangle | ShapeKind::Oval => {
                let edge = match self.kind {
                    ShapeKind::Rectangle => self.corners().to_vec(),
                    _ => self.ellipse(),
                };
                let fill = self
                    .fill
                    .paint()
                    .map(|ink| layer(Figure::Fill(edge.clone()), ink));
                let stroke = outline.map(|ink| {
                    let figure = Figure::Stroke {
                        points: edge,
                        closed: true,
                        width: self.width,
                    };
                    layer(figure, ink)
                });
                fill.into_iter().chain(stroke).collect()
            }
            ShapeKind::Line => outline
                .map(|ink| {
                    let figure = Figure::Stroke {
                        points: vec![self.start, self.end],
                        closed: false,
                        width: self.width,
                    };
                    layer(figure, ink)
                })
                .into_iter()
                .collect(),
            ShapeKind::Arrow => outline
                .zip(arrow(self.start, self.end, self.width))
                .map(|(ink, points)| layer(Figure::Fill(points), ink))
                .into_iter()
                .collect(),
        }
    }

    /// The box's corners, clockwise from the top left.
    fn corners(&self) -> [Point; 4] {
        let (x0, x1) = (self.start.0.min(self.end.0), self.start.0.max(self.end.0));
        let (y0, y1) = (self.start.1.min(self.end.1), self.start.1.max(self.end.1));
        [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
    }

    /// The oval in the box, as a polygon of steps about two pixels long.
    fn ellipse(&self) -> Vec<Point> {
        let (cx, cy) = (
            (self.start.0 + self.end.0) / 2.,
            (self.start.1 + self.end.1) / 2.,
        );
        let (rx, ry) = (
            (self.end.0 - self.start.0).abs() / 2.,
            (self.end.1 - self.start.1).abs() / 2.,
        );
        let steps = ((PI * (rx + ry) / 2.).ceil() as usize).clamp(16, 512);
        (0..steps)
            .map(|i| {
                let angle = i as f32 / steps as f32 * 2. * PI;
                (cx + rx * angle.cos(), cy + ry * angle.sin())
            })
            .collect()
    }

    /// Whether the shape comes within `reach` screenshot pixels of
    /// `point`: on its outline, or anywhere on its fill. A shape without a
    /// fill is hollow.
    pub fn touches(&self, point: Point, reach: f32) -> bool {
        self.layers().iter().any(|layer| match &layer.figure {
            Figure::Fill(polygon) => {
                inside(polygon, point)
                    || edges(polygon, true).any(|(a, b)| distance(point, a, b) <= reach)
            }
            Figure::Stroke {
                points,
                closed,
                width,
            } => edges(points, *closed).any(|(a, b)| distance(point, a, b) <= width / 2. + reach),
        })
    }
}

/// Where a drag from `start` to `end` ends with Shift held: a square or a
/// circle, the bigger side winning, from `start`; a line or an arrow at the
/// nearest 45°.
pub fn constrained(kind: ShapeKind, start: Point, end: Point) -> Point {
    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
    if kind.fills() {
        let side = dx.abs().max(dy.abs());
        return (start.0 + side * dx.signum(), start.1 + side * dy.signum());
    }
    let octant = (dy.atan2(dx) / FRAC_PI_4).round() as i32;
    match octant.rem_euclid(4) {
        0 => (end.0, start.1),
        2 => (start.0, end.1),
        _ => {
            let side = dx.abs().min(dy.abs());
            (start.0 + side * dx.signum(), start.1 + side * dy.signum())
        }
    }
}

/// An arrow from `tail` to `tip`, as one polygon: a shaft `width` wide and
/// a head five widths long and wide. One shorter than its head is all
/// head. `None` if it has no length.
fn arrow(tail: Point, tip: Point, width: f32) -> Option<Vec<Point>> {
    let (dx, dy) = (tip.0 - tail.0, tip.1 - tail.1);
    let length = dx.hypot(dy);
    if length < 0.01 {
        return None;
    }
    let (along, across) = ((dx / length, dy / length), (-dy / length, dx / length));
    let at = |forward: f32, side: f32| {
        (
            tail.0 + along.0 * forward + across.0 * side,
            tail.1 + along.1 * forward + across.1 * side,
        )
    };
    let (head, shaft) = (ARROW_HEAD * width / 2., width / 2.);
    let neck = length - ARROW_HEAD * width;
    if neck <= 0. {
        return Some(vec![at(0., head), tip, at(0., -head)]);
    }
    Some(vec![
        at(0., shaft),
        at(neck, shaft),
        at(neck, head),
        tip,
        at(neck, -head),
        at(neck, -shaft),
        at(0., -shaft),
    ])
}

/// The polygon's or line's edges, as pairs of points.
fn edges(points: &[Point], closed: bool) -> impl Iterator<Item = (Point, Point)> + '_ {
    let last = closed.then(|| (points[points.len() - 1], points[0]));
    points.windows(2).map(|pair| (pair[0], pair[1])).chain(last)
}

/// Whether `point` is inside `polygon` (even-odd).
fn inside(polygon: &[Point], (x, y): Point) -> bool {
    edges(polygon, true)
        .filter(|&((x0, y0), (x1, y1))| {
            (y0 > y) != (y1 > y) && x < x0 + (y - y0) / (y1 - y0) * (x1 - x0)
        })
        .count()
        % 2
        == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Rgb = Rgb(0xE6, 0x1B, 0x1B);
    const BLUE: Rgb = Rgb(0x00, 0x4D, 0xE6);

    fn shape(kind: ShapeKind, fill: Ink) -> Shape {
        Shape {
            kind,
            start: (10., 10.),
            end: (50., 30.),
            outline: ShapeStyle::DEFAULT.outline,
            fill,
            width: 4.,
        }
    }

    fn blue(opacity: u8) -> Ink {
        Ink {
            color: Some(BLUE),
            opacity,
        }
    }

    #[test]
    fn opacity_is_snipping_tools_share_of_255() {
        assert_eq!(blue(100).paint(), Some((BLUE, 255)));
        assert_eq!(blue(50).paint(), Some((BLUE, 127)));
        assert_eq!(blue(1).paint(), Some((BLUE, 2)));
        assert_eq!(Ink::TRANSPARENT.paint(), None);
    }

    #[test]
    fn the_colours_are_transparent_then_the_pens_less_iron_gray() {
        assert_eq!(SHAPE_COLORS[0], None);
        assert_eq!(SHAPE_COLORS[1], Some(PEN_COLORS[0]));
        assert!(!SHAPE_COLORS.contains(&Some(Rgb(0x58, 0x59, 0x5B))));
        assert_eq!(SHAPE_COLORS[29], Some(PEN_COLORS[29]));
        assert!(SHAPE_COLORS.contains(&ShapeStyle::DEFAULT.outline.color));
    }

    #[test]
    fn a_rectangle_is_filled_then_outlined_and_transparent_parts_are_left_out() {
        let layers = shape(ShapeKind::Rectangle, blue(50)).layers();
        assert_eq!(layers.len(), 2);
        assert_eq!(
            layers[0].figure,
            Figure::Fill(vec![(10., 10.), (50., 10.), (50., 30.), (10., 30.)])
        );
        assert_eq!((layers[0].color, layers[0].alpha), (BLUE, 127));
        assert!(matches!(
            layers[1].figure,
            Figure::Stroke {
                closed: true,
                width: 4.,
                ..
            }
        ));
        assert_eq!(layers[1].color, RED);
        // Without a fill, only the outline.
        assert_eq!(
            shape(ShapeKind::Rectangle, Ink::TRANSPARENT).layers().len(),
            1
        );
    }

    #[test]
    fn lines_and_arrows_have_no_fill() {
        let line = shape(ShapeKind::Line, blue(100)).layers();
        assert_eq!(line.len(), 1);
        assert_eq!(
            line[0].figure,
            Figure::Stroke {
                points: vec![(10., 10.), (50., 30.)],
                closed: false,
                width: 4.
            }
        );
        let arrow = shape(ShapeKind::Arrow, blue(100)).layers();
        assert_eq!(arrow.len(), 1);
        assert_eq!(arrow[0].color, RED);
    }

    #[test]
    fn an_arrows_head_is_five_widths_long_and_wide() {
        let points = arrow((0., 0.), (100., 0.), 4.).unwrap();
        assert_eq!(
            points,
            [
                (0., 2.),
                (80., 2.),
                (80., 10.),
                (100., 0.),
                (80., -10.),
                (80., -2.),
                (0., -2.)
            ]
        );
        // Shorter than its head: all head.
        assert_eq!(arrow((0., 0.), (10., 0.), 4.).unwrap().len(), 3);
        assert_eq!(arrow((5., 5.), (5., 5.), 4.), None);
    }

    #[test]
    fn an_oval_fits_its_box() {
        let oval = shape(ShapeKind::Oval, Ink::TRANSPARENT);
        let edge = oval.ellipse();
        assert!(edge.len() >= 16);
        for (x, y) in edge {
            assert!((10. ..=50.).contains(&x) && (10. ..=30.).contains(&y));
            // On the ellipse around (30, 20) with radii 20 and 10.
            let on = ((x - 30.) / 20.).powi(2) + ((y - 20.) / 10.).powi(2);
            assert!((on - 1.).abs() < 1e-3, "{on}");
        }
    }

    #[test]
    fn shift_makes_squares_and_snaps_lines_to_45_degrees() {
        use ShapeKind::*;
        // The bigger side wins, in the direction dragged.
        assert_eq!(constrained(Rectangle, (10., 10.), (40., 20.)), (40., 40.));
        assert_eq!(constrained(Oval, (10., 10.), (0., -30.)), (-30., -30.));
        // Nearly level, nearly upright, and diagonal (the smaller side).
        assert_eq!(constrained(Line, (0., 0.), (50., 8.)), (50., 0.));
        assert_eq!(constrained(Arrow, (0., 0.), (-6., -40.)), (0., -40.));
        assert_eq!(constrained(Line, (0., 0.), (-30., 26.)), (-26., 26.));
    }

    #[test]
    fn a_hollow_shape_is_touched_on_its_outline_and_a_filled_one_inside() {
        let hollow = shape(ShapeKind::Rectangle, Ink::TRANSPARENT);
        assert!(hollow.touches((30., 11.), 0.));
        assert!(!hollow.touches((30., 20.), 0.));
        assert!(hollow.touches((30., 6.), 3.));
        let filled = shape(ShapeKind::Rectangle, blue(20));
        assert!(filled.touches((30., 20.), 0.));
        assert!(!filled.touches((70., 20.), 3.));
        // An arrow is touched anywhere on it.
        let arrow = shape(ShapeKind::Arrow, Ink::TRANSPARENT);
        assert!(arrow.touches((30., 20.), 0.));
    }
}
