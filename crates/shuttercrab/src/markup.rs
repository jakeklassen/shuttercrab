//! Marks drawn on a screenshot in the main window, as in Snipping Tool:
//! pen and highlighter strokes. They are kept apart from the screenshot, in
//! its own pixels, until it is copied or saved; [`draw`] puts them on it.
//!
//! The pen paints over what is under it. The highlighter multiplies with
//! it, like highlighter ink: yellow on white stays yellow and text shows
//! through, and one colour over another darkens. A single stroke never
//! darkens where it crosses itself, since it is drawn in one go.

use tiny_skia::{
    BlendMode, Color, FillRule, IntSize, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect,
    Shader, Stroke as Outline, Transform,
};

/// What draws a stroke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Highlighter,
}

/// A colour, red, green and blue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// As `0xRRGGBB`.
    pub fn hex(self) -> u32 {
        (u32::from(self.0) << 16) | (u32::from(self.1) << 8) | u32::from(self.2)
    }
}

/// A tool's colour and size. The size is the stroke's width in logical
/// pixels of the screen the screenshot was taken on, so a size looks the
/// same against its text whatever that screen's scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Brush {
    pub color: Rgb,
    pub size: f32,
}

impl Brush {
    /// Snipping Tool's defaults: a red pen of 3, a yellow highlighter of 16.
    pub const PEN: Brush = Brush {
        color: Rgb(0xE6, 0x1B, 0x1B),
        size: 3.,
    };
    pub const HIGHLIGHTER: Brush = Brush {
        color: Rgb(0xFF, 0xE6, 0x00),
        size: 16.,
    };
}

/// One stroke, in screenshot pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub tool: Tool,
    pub color: Rgb,
    /// Its width, screenshot pixels.
    pub width: f32,
    pub points: Vec<(f32, f32)>,
}

/// The marks on a screenshot, with what undo took off for redo.
#[derive(Clone, Debug, Default)]
pub struct Marks {
    strokes: Vec<Stroke>,
    undone: Vec<Stroke>,
}

impl Marks {
    pub fn strokes(&self) -> &[Stroke] {
        &self.strokes
    }

    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty()
    }

    /// Add a finished stroke. What undo took off can no longer be redone.
    pub fn add(&mut self, stroke: Stroke) {
        self.strokes.push(stroke);
        self.undone.clear();
    }

    /// Take off the latest stroke. Returns whether there was one.
    pub fn undo(&mut self) -> bool {
        match self.strokes.pop() {
            Some(stroke) => {
                self.undone.push(stroke);
                true
            }
            None => false,
        }
    }

    /// Put back the stroke undo took off last. Returns whether there was one.
    pub fn redo(&mut self) -> bool {
        match self.undone.pop() {
            Some(stroke) => {
                self.strokes.push(stroke);
                true
            }
            None => false,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.strokes.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }
}

/// `strokes` drawn onto a `width` × `height` screenshot. `pixels` and the
/// result are four bytes a pixel with straight alpha, in either channel
/// order; `bgr` says the first byte is blue (as GPUI draws them), so the
/// stroke colours are swapped to match. Pen and multiply blending treat
/// each channel alike, which makes the order free.
pub fn draw(pixels: &[u8], width: u32, height: u32, strokes: &[Stroke], bgr: bool) -> Vec<u8> {
    let mut data = pixels.to_vec();
    premultiply(&mut data);
    let size = IntSize::from_wh(width, height).expect("a screenshot has an area");
    let mut canvas = Pixmap::from_vec(data, size).expect("the buffer matches its size");
    for stroke in strokes {
        paint(&mut canvas, stroke, bgr);
    }
    let mut data = canvas.take();
    demultiply(&mut data);
    data
}

/// Draw one stroke onto the canvas.
fn paint(canvas: &mut Pixmap, stroke: &Stroke, bgr: bool) {
    let Rgb(r, g, b) = stroke.color;
    let (first, third) = if bgr { (b, r) } else { (r, b) };
    let mut paint = Paint {
        shader: Shader::SolidColor(Color::from_rgba8(first, g, third, 255)),
        anti_alias: true,
        ..Paint::default()
    };
    if stroke.tool == Tool::Highlighter {
        paint.blend_mode = BlendMode::Multiply;
    }
    let half = stroke.width / 2.;
    match stroke.points.as_slice() {
        [] => {}
        // A click leaves a dot: round for the pen, square for the
        // highlighter's flat tip.
        [(x, y)] => {
            let dot = match stroke.tool {
                Tool::Pen => PathBuilder::from_circle(*x, *y, half),
                Tool::Highlighter => {
                    Rect::from_xywh(x - half, y - half, stroke.width, stroke.width)
                        .map(PathBuilder::from_rect)
                }
            };
            if let Some(dot) = dot {
                canvas.fill_path(&dot, &paint, FillRule::Winding, Transform::identity(), None);
            }
        }
        points => {
            let Some(path) = smooth_path(points) else {
                return;
            };
            let outline = Outline {
                width: stroke.width,
                line_cap: match stroke.tool {
                    Tool::Pen => LineCap::Round,
                    Tool::Highlighter => LineCap::Square,
                },
                line_join: LineJoin::Round,
                ..Outline::default()
            };
            canvas.stroke_path(&path, &paint, &outline, Transform::identity(), None);
        }
    }
}

/// A smooth path through `points`: straight to the first midpoint, then a
/// curve through each point to the next midpoint, and straight to the end.
fn smooth_path(points: &[(f32, f32)]) -> Option<tiny_skia::Path> {
    let mut path = PathBuilder::new();
    let (x0, y0) = points[0];
    path.move_to(x0, y0);
    for pair in points.windows(2).skip(1) {
        let ((cx, cy), (nx, ny)) = (pair[0], pair[1]);
        path.quad_to(cx, cy, (cx + nx) / 2., (cy + ny) / 2.);
    }
    let (xn, yn) = points[points.len() - 1];
    path.line_to(xn, yn);
    path.finish()
}

/// Straight alpha to premultiplied, as `tiny-skia` draws. A screenshot is
/// opaque but for Freeform's outside, which is fully transparent.
fn premultiply(data: &mut [u8]) {
    for px in data.as_chunks_mut::<4>().0 {
        let a = u16::from(px[3]);
        if a < 255 {
            for c in &mut px[..3] {
                *c = ((u16::from(*c) * a + 127) / 255) as u8;
            }
        }
    }
}

fn demultiply(data: &mut [u8]) {
    for px in data.as_chunks_mut::<4>().0 {
        let a = u16::from(px[3]);
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((u16::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A white `size` × `size` RGBA screenshot.
    fn white(size: u32) -> Vec<u8> {
        vec![255; (size * size * 4) as usize]
    }

    fn pixel(data: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * size + x) * 4) as usize;
        data[at..at + 4].try_into().unwrap()
    }

    fn line(tool: Tool, color: Rgb, y: f32) -> Stroke {
        Stroke {
            tool,
            color,
            width: 6.,
            points: vec![(2., y), (10., y), (18., y)],
        }
    }

    #[test]
    fn the_pen_paints_over_and_leaves_the_rest() {
        let red = Rgb(230, 27, 27);
        let out = draw(&white(20), 20, 20, &[line(Tool::Pen, red, 10.)], false);
        assert_eq!(pixel(&out, 20, 10, 10), [230, 27, 27, 255]);
        assert_eq!(pixel(&out, 20, 10, 2), [255, 255, 255, 255]);
    }

    #[test]
    fn the_highlighter_multiplies() {
        let yellow = Rgb(255, 230, 0);
        let green = Rgb(38, 230, 0);
        let strokes = [
            line(Tool::Highlighter, yellow, 10.),
            line(Tool::Highlighter, green, 10.),
        ];
        let out = draw(&white(20), 20, 20, &strokes, false);
        // Yellow on white, then green over it: each channel multiplied.
        let [r, g, b, a] = pixel(&out, 20, 10, 10);
        assert_eq!((r, b, a), (38, 0, 255));
        assert!(g.abs_diff(207) <= 1, "{g}");
    }

    #[test]
    fn a_crossing_highlighter_stroke_does_not_darken_itself() {
        let cyan = Rgb(0, 170, 204);
        let cross = Stroke {
            tool: Tool::Highlighter,
            color: cyan,
            width: 6.,
            points: vec![(2., 2.), (18., 18.), (18., 2.), (2., 18.)],
        };
        let out = draw(&white(20), 20, 20, &[cross], false);
        assert_eq!(pixel(&out, 20, 10, 10), [0, 170, 204, 255]);
    }

    #[test]
    fn colours_follow_the_channel_order() {
        let red = Rgb(230, 27, 27);
        let out = draw(&white(20), 20, 20, &[line(Tool::Pen, red, 10.)], true);
        assert_eq!(pixel(&out, 20, 10, 10), [27, 27, 230, 255]);
    }

    #[test]
    fn transparency_outside_freeform_stays() {
        let mut clear = white(20);
        for px in clear.as_chunks_mut::<4>().0 {
            *px = [0, 0, 0, 0];
        }
        let out = draw(&clear, 20, 20, &[], false);
        assert_eq!(pixel(&out, 20, 5, 5), [0, 0, 0, 0]);
    }

    #[test]
    fn undo_and_redo_walk_the_strokes() {
        let mut marks = Marks::default();
        let stroke = line(Tool::Pen, Rgb(0, 0, 0), 5.);
        marks.add(stroke.clone());
        marks.add(stroke.clone());
        assert!(marks.undo());
        assert_eq!(marks.strokes().len(), 1);
        assert!(marks.redo());
        assert!(!marks.redo());
        assert_eq!(marks.strokes().len(), 2);
        marks.undo();
        // A new stroke ends what can be redone.
        marks.add(stroke);
        assert!(!marks.can_redo());
        assert!(marks.undo() && marks.undo());
        assert!(!marks.undo());
    }
}
