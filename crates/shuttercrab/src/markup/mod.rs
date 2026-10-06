//! Marks drawn on a screenshot in the main window, as in Snipping Tool:
//! pen and highlighter strokes, and shapes. They are kept apart from the screenshot, in
//! its own pixels, until it is copied or saved; [`draw`] puts them on it.
//! Each stays a mark of its own, so the eraser and undo take it off whole.
//!
//! The pen paints over what is under it, with a round tip. The highlighter
//! has a slanted chisel tip, so a stroke across has slanted ends, and it
//! multiplies with what is under it, like highlighter ink: yellow on white stays yellow and text shows
//! through, and one colour over another darkens. A single stroke never
//! darkens where it crosses itself, since it is drawn in one go.

mod emoji;
mod shape;

pub use emoji::{Emoji, image as emoji_image};

pub use shape::{
    EMOJI_MAX, Figure, Ink, LEAST_DRAG, Layer, SHAPE_COLORS, Shape, ShapeKind, ShapeStyle,
    constrained,
};
use tiny_skia::{
    BlendMode, Color, FillRule, IntSize, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Shader,
    Stroke as Outline, Transform,
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

/// The pen's colours, Snipping Tool's 30 in its 6 × 5 grid: greys, brights,
/// greens and blues, purples and browns, pastels.
pub const PEN_COLORS: [Rgb; 30] = [
    Rgb(0x00, 0x00, 0x00),
    Rgb(0xFF, 0xFF, 0xFF),
    Rgb(0xD1, 0xD3, 0xD4),
    Rgb(0xA7, 0xA9, 0xAC),
    Rgb(0x80, 0x82, 0x85),
    Rgb(0x58, 0x59, 0x5B),
    Rgb(0xB3, 0x15, 0x64),
    Rgb(0xE6, 0x1B, 0x1B),
    Rgb(0xFF, 0x55, 0x00),
    Rgb(0xFF, 0xAA, 0x00),
    Rgb(0xFF, 0xCE, 0x00),
    Rgb(0xFF, 0xE6, 0x00),
    Rgb(0xA2, 0xE6, 0x1B),
    Rgb(0x26, 0xE6, 0x00),
    Rgb(0x00, 0x80, 0x55),
    Rgb(0x00, 0xAA, 0xCC),
    Rgb(0x00, 0x4D, 0xE6),
    Rgb(0x3D, 0x00, 0xB8),
    Rgb(0x66, 0x00, 0xCC),
    Rgb(0x60, 0x00, 0x80),
    Rgb(0xF7, 0xD7, 0xC4),
    Rgb(0xBB, 0x91, 0x67),
    Rgb(0x8E, 0x56, 0x2E),
    Rgb(0x61, 0x3D, 0x30),
    Rgb(0xFF, 0x80, 0xFF),
    Rgb(0xFF, 0xC6, 0x80),
    Rgb(0xFF, 0xFF, 0x80),
    Rgb(0x80, 0xFF, 0x9E),
    Rgb(0x80, 0xD6, 0xFF),
    Rgb(0xBC, 0xB3, 0xFF),
];

/// The highlighter's colours, Snipping Tool's 6: yellow, green, blue, pink,
/// orange, purple.
pub const HIGHLIGHTER_COLORS: [Rgb; 6] = [
    Rgb(0xFF, 0xE6, 0x00),
    Rgb(0x26, 0xE6, 0x00),
    Rgb(0x44, 0xC8, 0xF5),
    Rgb(0xEC, 0x00, 0x8C),
    Rgb(0xFF, 0x55, 0x00),
    Rgb(0x66, 0x00, 0xCC),
];

impl Tool {
    /// The colours it offers.
    pub fn colors(self) -> &'static [Rgb] {
        match self {
            Tool::Pen => &PEN_COLORS,
            Tool::Highlighter => &HIGHLIGHTER_COLORS,
        }
    }

    /// Its sizes, smallest to largest, as Snipping Tool's.
    pub fn sizes(self) -> std::ops::RangeInclusive<f32> {
        match self {
            Tool::Pen => 1. ..=24.,
            Tool::Highlighter => 12. ..=64.,
        }
    }

    pub fn default_brush(self) -> Brush {
        match self {
            Tool::Pen => Brush::PEN,
            Tool::Highlighter => Brush::HIGHLIGHTER,
        }
    }
}

impl Rgb {
    /// As `#RRGGBB`, as settings keep it.
    pub fn to_hex_string(self) -> String {
        format!("#{:06X}", self.hex())
    }

    /// From `#RRGGBB`.
    pub fn from_hex_string(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('#')?;
        if digits.len() != 6 {
            return None;
        }
        let value = u32::from_str_radix(digits, 16).ok()?;
        Some(Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8))
    }
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

/// A stroke being drawn, point by point.
#[derive(Clone, Debug, PartialEq)]
pub struct Drawing {
    pub stroke: Stroke,
    /// While Shift is held, where the straight part starts: an index into
    /// the points.
    anchor: Option<usize>,
}

impl Drawing {
    pub fn new(stroke: Stroke) -> Self {
        Self {
            stroke,
            anchor: None,
        }
    }

    /// Carry the stroke on to `point`. With `straight` (Shift held), it
    /// runs straight to `point` from where Shift went down, or from where
    /// it started if Shift was down already; without, it goes freehand.
    pub fn extend_to(&mut self, point: (f32, f32), straight: bool) {
        let points = &mut self.stroke.points;
        if !straight {
            self.anchor = None;
            let (lx, ly) = points[points.len() - 1];
            // Half a screenshot pixel apart at least: enough for a smooth
            // line, without piling up points.
            if (point.0 - lx).hypot(point.1 - ly) >= 0.5 {
                points.push(point);
            }
            return;
        }
        let anchor = *self.anchor.get_or_insert(points.len() - 1);
        points.truncate(anchor + 1);
        points.push(point);
    }
}

/// One mark on a screenshot.
#[derive(Clone, Debug, PartialEq)]
pub enum Mark {
    Stroke(Stroke),
    Shape(Shape),
}

impl Mark {
    /// The stroke, if the mark is one.
    pub fn as_stroke(&self) -> Option<&Stroke> {
        match self {
            Mark::Stroke(stroke) => Some(stroke),
            Mark::Shape(_) => None,
        }
    }

    /// The shape, if the mark is one.
    pub fn as_shape(&self) -> Option<&Shape> {
        match self {
            Mark::Shape(shape) => Some(shape),
            Mark::Stroke(_) => None,
        }
    }

    /// Whether the mark comes within `reach` screenshot pixels of `point`.
    pub fn touches(&self, point: (f32, f32), reach: f32) -> bool {
        match self {
            Mark::Stroke(stroke) => stroke.touches(point, reach),
            Mark::Shape(shape) => shape.touches(point, reach),
        }
    }
}

/// The marks on a screenshot, with the changes made to them, for undo, and
/// the changes undo took back, for redo.
#[derive(Clone, Debug, Default)]
pub struct Marks {
    marks: Vec<Mark>,
    done: Vec<Change>,
    undone: Vec<Change>,
}

/// One change to the marks, as undo takes it back whole.
#[derive(Clone, Debug)]
enum Change {
    Added(Mark),
    /// The marks the eraser took in one drag, each with the place it had
    /// when taken, in the order taken.
    Erased(Vec<(usize, Mark)>),
    /// Every mark, by Erase all mark-ups.
    Cleared(Vec<Mark>),
    /// The mark at `index`, changed in place: moved, resized, turned or
    /// recoloured.
    Edited {
        index: usize,
        before: Mark,
        after: Mark,
        how: Edit,
    },
}

/// How an edit joins the one before it, for undo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    /// On its own: a drag, a turn from the menu.
    Once,
    /// One of a run of key presses (moving, resizing, turning by key),
    /// undone together.
    Nudge,
    /// One of a run of colour, opacity and size changes, undone together.
    Style,
}

impl Marks {
    pub fn marks(&self) -> &[Mark] {
        &self.marks
    }

    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }

    /// Add a finished mark.
    pub fn add(&mut self, mark: Mark) {
        self.marks.push(mark.clone());
        self.record(Change::Added(mark));
    }

    /// Erase the mark at `index`. With `joining`, it joins the eraser's
    /// last change, so one drag is undone in one go.
    pub fn erase(&mut self, index: usize, joining: bool) {
        let mark = self.marks.remove(index);
        if joining && let Some(Change::Erased(taken)) = self.done.last_mut() {
            taken.push((index, mark));
            return;
        }
        self.record(Change::Erased(vec![(index, mark)]));
    }

    /// Erase every mark. Returns whether there were any.
    pub fn clear(&mut self) -> bool {
        if self.marks.is_empty() {
            return false;
        }
        let all = std::mem::take(&mut self.marks);
        self.record(Change::Cleared(all));
        true
    }

    /// Put `mark` in place of the one at `index`. A `Nudge` or `Style`
    /// edit joins the last change if that was the same kind of edit to the
    /// same mark.
    pub fn edit(&mut self, index: usize, mark: Mark, how: Edit) {
        let before = std::mem::replace(&mut self.marks[index], mark.clone());
        if how != Edit::Once
            && let Some(Change::Edited {
                index: last,
                after,
                how: last_how,
                ..
            }) = self.done.last_mut()
            && *last == index
            && *last_how == how
        {
            *after = mark;
            self.undone.clear();
            return;
        }
        self.record(Change::Edited {
            index,
            before,
            after: mark,
            how,
        });
    }

    /// The mark undo would change in place next, if it would edit one.
    pub fn undo_edits(&self) -> Option<usize> {
        match self.done.last()? {
            Change::Edited { index, .. } => Some(*index),
            _ => None,
        }
    }

    /// The mark redo would change in place next, if it would edit one.
    pub fn redo_edits(&self) -> Option<usize> {
        match self.undone.last()? {
            Change::Edited { index, .. } => Some(*index),
            _ => None,
        }
    }

    /// A new change: what undo took back can no longer be redone.
    fn record(&mut self, change: Change) {
        self.done.push(change);
        self.undone.clear();
    }

    /// Take back the latest change. Returns whether there was one.
    pub fn undo(&mut self) -> bool {
        let Some(change) = self.done.pop() else {
            return false;
        };
        match &change {
            Change::Added(_) => {
                self.marks.pop();
            }
            Change::Erased(taken) => {
                for (index, mark) in taken.iter().rev() {
                    self.marks.insert(*index, mark.clone());
                }
            }
            Change::Cleared(all) => self.marks = all.clone(),
            Change::Edited { index, before, .. } => self.marks[*index] = before.clone(),
        }
        self.undone.push(change);
        true
    }

    /// Make again the change undo took back last. Returns whether there was
    /// one.
    pub fn redo(&mut self) -> bool {
        let Some(change) = self.undone.pop() else {
            return false;
        };
        match &change {
            Change::Added(mark) => self.marks.push(mark.clone()),
            Change::Erased(taken) => {
                for (index, _) in taken {
                    self.marks.remove(*index);
                }
            }
            Change::Cleared(_) => self.marks.clear(),
            Change::Edited { index, after, .. } => self.marks[*index] = after.clone(),
        }
        self.done.push(change);
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }
}

impl Stroke {
    /// Whether the stroke comes within `reach` screenshot pixels of
    /// `point`, counting its own width.
    pub fn touches(&self, point: (f32, f32), reach: f32) -> bool {
        let near = self.width / 2. + reach;
        let samples = smooth_points(&self.points);
        if let [only] = samples.as_slice() {
            return distance(point, *only, *only) <= near;
        }
        samples
            .windows(2)
            .any(|pair| distance(point, pair[0], pair[1]) <= near)
    }
}

/// How far `point` is from the segment `a`–`b`.
fn distance(point: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length = dx * dx + dy * dy;
    let t = if length == 0. {
        0.
    } else {
        (((point.0 - a.0) * dx + (point.1 - a.1) * dy) / length).clamp(0., 1.)
    };
    (point.0 - (a.0 + t * dx)).hypot(point.1 - (a.1 + t * dy))
}

/// `marks` drawn onto a `width` × `height` screenshot. `pixels` and the
/// result are four bytes a pixel with straight alpha, in either channel
/// order; `bgr` says the first byte is blue (as GPUI draws them), so the
/// mark colours are swapped to match. Painting over and multiply blending
/// treat each channel alike, which makes the order free.
pub fn draw(pixels: &[u8], width: u32, height: u32, marks: &[Mark], bgr: bool) -> Vec<u8> {
    let whole = Region {
        x: 0,
        y: 0,
        width,
        height,
    };
    draw_region(pixels, width, whole, marks, bgr)
}

/// A rectangle of a screenshot, pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Stroke {
    /// The part of a `width` × `height` screenshot the stroke can touch;
    /// `None` if it lies wholly outside.
    pub fn region(&self, width: u32, height: u32) -> Option<Region> {
        // Half the width each side, more for the highlighter's square
        // corners, and a pixel for smoothing.
        let reach = self.width * 0.75 + 1.;
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in &self.points {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        let left = (x0 - reach).floor().max(0.) as u32;
        let top = (y0 - reach).floor().max(0.) as u32;
        let right = ((x1 + reach).ceil().max(0.) as u32).min(width);
        let bottom = ((y1 + reach).ceil().max(0.) as u32).min(height);
        (left < right && top < bottom).then(|| Region {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        })
    }
}

/// `marks` drawn onto `region` of a screenshot `width` pixels wide, as
/// [`draw`] draws them, giving only that region: enough to show a stroke
/// being drawn without drawing the whole screenshot each time.
pub fn draw_region(
    pixels: &[u8],
    width: u32,
    region: Region,
    marks: &[Mark],
    bgr: bool,
) -> Vec<u8> {
    let (row, cut) = (width as usize * 4, region.width as usize * 4);
    let mut data = Vec::with_capacity(cut * region.height as usize);
    for y in region.y..region.y + region.height {
        let start = y as usize * row + region.x as usize * 4;
        data.extend_from_slice(&pixels[start..start + cut]);
    }
    premultiply(&mut data);
    let size = IntSize::from_wh(region.width, region.height).expect("a region has an area");
    let mut canvas = Pixmap::from_vec(data, size).expect("the buffer matches its size");
    let shift = Transform::from_translate(-(region.x as f32), -(region.y as f32));
    for mark in marks {
        match mark {
            Mark::Stroke(stroke) => paint(&mut canvas, stroke, bgr, shift),
            Mark::Shape(shape) => match shape.kind {
                ShapeKind::Emoji(emoji) => {
                    let side = (shape.end.0 - shape.start.0).abs();
                    let placed = (shape.center(), side, shape.angle);
                    emoji::paint(&mut canvas, emoji, placed, bgr, shift);
                }
                _ => paint_shape(&mut canvas, shape, bgr, shift),
            },
        }
    }
    let mut data = canvas.take();
    demultiply(&mut data);
    data
}

/// Draw one stroke onto the canvas, moved by `shift`.
fn paint(canvas: &mut Pixmap, stroke: &Stroke, bgr: bool, shift: Transform) {
    let Rgb(r, g, b) = stroke.color;
    let (first, third) = if bgr { (b, r) } else { (r, b) };
    let mut paint = Paint {
        shader: Shader::SolidColor(Color::from_rgba8(first, g, third, 255)),
        anti_alias: true,
        ..Paint::default()
    };
    if stroke.tool == Tool::Highlighter {
        // One fill for the whole swept shape: it multiplies once, so the
        // stroke never darkens where it crosses itself.
        paint.blend_mode = BlendMode::Multiply;
        if let Some(path) = chisel_path(&stroke.points, stroke.width) {
            canvas.fill_path(&path, &paint, FillRule::Winding, shift, None);
        }
        return;
    }
    match stroke.points.as_slice() {
        [] => {}
        // A click leaves a dot.
        [(x, y)] => {
            if let Some(dot) = PathBuilder::from_circle(*x, *y, stroke.width / 2.) {
                canvas.fill_path(&dot, &paint, FillRule::Winding, shift, None);
            }
        }
        points => {
            let Some(path) = smooth_path(points) else {
                return;
            };
            let outline = Outline {
                width: stroke.width,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Outline::default()
            };
            canvas.stroke_path(&path, &paint, &outline, shift, None);
        }
    }
}

/// Draw one shape onto the canvas, moved by `shift`: each of its layers
/// painted over what is under it, as see-through as its ink.
fn paint_shape(canvas: &mut Pixmap, shape: &Shape, bgr: bool, shift: Transform) {
    for layer in shape.layers() {
        let Rgb(r, g, b) = layer.color;
        let (first, third) = if bgr { (b, r) } else { (r, b) };
        let paint = Paint {
            shader: Shader::SolidColor(Color::from_rgba8(first, g, third, layer.alpha)),
            anti_alias: true,
            ..Paint::default()
        };
        match &layer.figure {
            Figure::Fill(points) => {
                if let Some(path) = polygon(points, true) {
                    canvas.fill_path(&path, &paint, FillRule::Winding, shift, None);
                }
            }
            Figure::Stroke {
                points,
                closed,
                width,
            } => {
                let outline = Outline {
                    width: *width,
                    line_cap: LineCap::Butt,
                    line_join: LineJoin::Miter,
                    ..Outline::default()
                };
                if let Some(path) = polygon(points, *closed) {
                    canvas.stroke_path(&path, &paint, &outline, shift, None);
                }
            }
        }
    }
}

/// Straight lines through `points`, back to the first if `closed`.
fn polygon(points: &[(f32, f32)], closed: bool) -> Option<tiny_skia::Path> {
    let (&(x0, y0), rest) = points.split_first()?;
    let mut path = PathBuilder::new();
    path.move_to(x0, y0);
    for &(x, y) in rest {
        path.line_to(x, y);
    }
    if closed {
        path.close();
    }
    path.finish()
}

/// How far the highlighter's tip leans from upright: the tangent of 30°.
const CHISEL_LEAN: f32 = 0.577;

/// The highlighter's chisel tip around its centre, as corners: a bar
/// `size` tall leaning right like `/`, a fifth of that thick. Drawn across,
/// it leaves a band with slanted ends, `/====/`; along its slant, a thin
/// line.
pub fn chisel(size: f32) -> [(f32, f32); 4] {
    let half = size / 2.;
    let lean = half * CHISEL_LEAN;
    let thick = size / 10.;
    [
        (-lean - thick, half),
        (lean - thick, -half),
        (lean + thick, -half),
        (-lean + thick, half),
    ]
}

/// The shape the chisel tip sweeps along the smoothed `points`: for each
/// step, the convex hull of the tip at its two ends. All the hulls wind the
/// same way, so filled together they make their union.
fn chisel_path(points: &[(f32, f32)], size: f32) -> Option<tiny_skia::Path> {
    let tip = chisel(size);
    let at = |(x, y): (f32, f32)| tip.map(|(tx, ty)| (x + tx, y + ty));
    let samples = smooth_points(points);
    let mut path = PathBuilder::new();
    let mut add = |corners: &[(f32, f32)]| {
        let (x0, y0) = corners[0];
        path.move_to(x0, y0);
        for &(x, y) in &corners[1..] {
            path.line_to(x, y);
        }
        path.close();
    };
    match samples.as_slice() {
        [] => return None,
        [one] => add(&convex_hull(&at(*one))),
        _ => {
            for pair in samples.windows(2) {
                let mut corners = at(pair[0]).to_vec();
                corners.extend(at(pair[1]));
                add(&convex_hull(&corners));
            }
        }
    }
    path.finish()
}

/// Points along the curve [`smooth_path`] draws through `points`, close
/// enough together to sweep a tip along.
fn smooth_points(points: &[(f32, f32)]) -> Vec<(f32, f32)> {
    const STEPS: usize = 4;
    let mut out = vec![points[0]];
    let mut from = points[0];
    for pair in points.windows(2).skip(1) {
        let (c, n) = (pair[0], pair[1]);
        let to = ((c.0 + n.0) / 2., (c.1 + n.1) / 2.);
        for step in 1..=STEPS {
            let t = step as f32 / STEPS as f32;
            let u = 1. - t;
            out.push((
                u * u * from.0 + 2. * u * t * c.0 + t * t * to.0,
                u * u * from.1 + 2. * u * t * c.1 + t * t * to.1,
            ));
        }
        from = to;
    }
    if points.len() > 1 {
        out.push(points[points.len() - 1]);
    }
    out
}

/// The convex hull of `points`, always wound the same way (Andrew's
/// monotone chain).
fn convex_hull(points: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let mut sorted = points.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut lower = hull_half(sorted.iter().copied());
    let mut upper = hull_half(sorted.iter().rev().copied());
    // Each half ends where the other begins.
    lower.pop();
    upper.pop();
    lower.append(&mut upper);
    lower
}

/// One half of a convex hull, from points in order: each kept point turns
/// the same way from the two before it.
fn hull_half(points: impl Iterator<Item = (f32, f32)>) -> Vec<(f32, f32)> {
    let turn = |o: (f32, f32), a: (f32, f32), b: (f32, f32)| {
        (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
    };
    let mut half: Vec<(f32, f32)> = Vec::new();
    for p in points {
        while half.len() >= 2 && turn(half[half.len() - 2], half[half.len() - 1], p) <= 0. {
            half.pop();
        }
        half.push(p);
    }
    half
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
        let out = draw(
            &white(20),
            20,
            20,
            &[Mark::Stroke(line(Tool::Pen, red, 10.))],
            false,
        );
        assert_eq!(pixel(&out, 20, 10, 10), [230, 27, 27, 255]);
        assert_eq!(pixel(&out, 20, 10, 2), [255, 255, 255, 255]);
    }

    #[test]
    fn the_highlighter_multiplies() {
        let yellow = Rgb(255, 230, 0);
        let green = Rgb(38, 230, 0);
        let strokes = [
            Mark::Stroke(line(Tool::Highlighter, yellow, 10.)),
            Mark::Stroke(line(Tool::Highlighter, green, 10.)),
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
        let out = draw(&white(20), 20, 20, &[Mark::Stroke(cross)], false);
        assert_eq!(pixel(&out, 20, 10, 10), [0, 170, 204, 255]);
    }

    #[test]
    fn colours_follow_the_channel_order() {
        let red = Rgb(230, 27, 27);
        let out = draw(
            &white(20),
            20,
            20,
            &[Mark::Stroke(line(Tool::Pen, red, 10.))],
            true,
        );
        assert_eq!(pixel(&out, 20, 10, 10), [27, 27, 230, 255]);
    }

    #[test]
    fn a_shape_is_outlined_over_its_see_through_fill() {
        let rectangle = Shape {
            kind: ShapeKind::Rectangle,
            start: (4., 4.),
            end: (16., 16.),
            outline: ShapeStyle::DEFAULT.outline,
            fill: Ink {
                color: Some(Rgb(0, 0, 255)),
                opacity: 50,
            },
            width: 2.,
            angle: 0.,
        };
        let out = draw(&white(20), 20, 20, &[Mark::Shape(rectangle)], false);
        // The outline, opaque red, centred on the edge.
        assert_eq!(pixel(&out, 20, 10, 4), [230, 27, 27, 255]);
        // Half-opaque blue over white inside; untouched outside.
        let [r, g, b, a] = pixel(&out, 20, 10, 10);
        assert!(r.abs_diff(128) <= 1 && g.abs_diff(128) <= 1, "{r} {g}");
        assert_eq!((b, a), (255, 255));
        assert_eq!(pixel(&out, 20, 1, 1), [255, 255, 255, 255]);
    }

    #[test]
    fn an_emoji_is_drawn_in_its_box_and_nowhere_else() {
        let star = Shape {
            kind: ShapeKind::Emoji(Emoji::Star),
            start: (4., 4.),
            end: (28., 28.),
            outline: Ink::TRANSPARENT,
            fill: Ink::TRANSPARENT,
            width: 0.,
            angle: 0.,
        };
        let out = draw(&white(32), 32, 32, &[Mark::Shape(star)], false);
        // The star's middle is yellow-orange: much more red than blue.
        let [r, _, b, a] = pixel(&out, 32, 16, 16);
        assert!(r > 200 && b < 120 && a == 255, "{r} {b}");
        assert_eq!(pixel(&out, 32, 1, 1), [255, 255, 255, 255]);
        assert_eq!(pixel(&out, 32, 30, 30), [255, 255, 255, 255]);
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
    fn a_region_matches_the_same_part_of_the_whole() {
        // A grey ramp, so every pixel differs.
        let base: Vec<u8> = (0..40 * 30)
            .flat_map(|i| [(i % 251) as u8, (i % 97) as u8, (i % 13) as u8, 255])
            .collect();
        let stroke = Stroke {
            tool: Tool::Highlighter,
            color: Rgb(255, 230, 0),
            width: 5.,
            points: vec![(8., 6.), (20., 15.), (30., 9.)],
        };
        let whole = draw(&base, 40, 30, &[Mark::Stroke(stroke.clone())], false);
        let region = stroke.region(40, 30).unwrap();
        let part = draw_region(&base, 40, region, &[Mark::Stroke(stroke)], false);
        for y in 0..region.height {
            for x in 0..region.width {
                let from = (((region.y + y) * 40 + region.x + x) * 4) as usize;
                let at = ((y * region.width + x) * 4) as usize;
                assert_eq!(whole[from..from + 4], part[at..at + 4], "{x},{y}");
            }
        }
    }

    #[test]
    fn shift_draws_straight_from_where_it_went_down() {
        let mut drawing = Drawing::new(Stroke {
            tool: Tool::Pen,
            color: Rgb(0, 0, 0),
            width: 3.,
            points: vec![(0., 0.)],
        });
        drawing.extend_to((5., 5.), false);
        drawing.extend_to((10., 3.), false);
        // Shift down: straight from (10, 3), wherever the pointer wanders.
        drawing.extend_to((20., 20.), true);
        drawing.extend_to((30., 10.), true);
        assert_eq!(
            drawing.stroke.points,
            [(0., 0.), (5., 5.), (10., 3.), (30., 10.)]
        );
        // Shift up: freehand again from there; down again: a new anchor.
        drawing.extend_to((32., 12.), false);
        drawing.extend_to((40., 40.), true);
        drawing.extend_to((50., 20.), true);
        assert_eq!(
            drawing.stroke.points[3..],
            [(30., 10.), (32., 12.), (50., 20.)]
        );
        // Close points are skipped freehand.
        let before = drawing.stroke.points.len();
        drawing.extend_to((50.2, 20.1), false);
        assert_eq!(drawing.stroke.points.len(), before);
    }

    #[test]
    fn shift_from_the_start_draws_one_straight_line() {
        let mut drawing = Drawing::new(Stroke {
            tool: Tool::Highlighter,
            color: Rgb(255, 230, 0),
            width: 16.,
            points: vec![(1., 1.)],
        });
        for x in 2..30 {
            drawing.extend_to((x as f32, (x * 3 % 7) as f32), true);
        }
        assert_eq!(drawing.stroke.points, [(1., 1.), (29., 3.)]);
    }

    #[test]
    fn the_highlighter_draws_a_band_with_slanted_ends() {
        // A size-12 stroke across a 60 × 30 white screenshot.
        let stroke = Stroke {
            tool: Tool::Highlighter,
            color: Rgb(0, 0, 0),
            width: 12.,
            points: vec![(15., 15.), (45., 15.)],
        };
        let out = draw(&white(60), 60, 60, &[Mark::Stroke(stroke)], false);
        let inked = |x: u32, y: u32| pixel(&out, 60, x, y)[0] < 128;
        // Full height in the middle.
        assert!(inked(30, 10) && inked(30, 20));
        // The ends lean like `/`: the start reaches further left at the
        // bottom, the end further right at the top.
        assert!(inked(13, 19) && !inked(13, 11));
        assert!(inked(47, 11) && !inked(47, 19));
    }

    #[test]
    fn a_hull_holds_every_point() {
        let points = [(0., 0.), (4., 1.), (2., 2.), (1., 4.), (3., 3.), (4., 4.)];
        let hull = convex_hull(&points);
        assert_eq!(hull.len(), 4);
        for corner in [(0., 0.), (4., 1.), (4., 4.), (1., 4.)] {
            assert!(hull.contains(&corner), "{corner:?}");
        }
    }

    #[test]
    fn erasing_is_undone_in_one_go_and_redone() {
        let mut marks = Marks::default();
        for y in [5., 10., 15.] {
            marks.add(Mark::Stroke(line(Tool::Pen, Rgb(0, 0, 0), y)));
        }
        // One drag takes the first and, joining, the (new) first again.
        marks.erase(0, false);
        marks.erase(0, true);
        assert_eq!(marks.marks().len(), 1);
        let heights = |marks: &Marks| -> Vec<f32> {
            marks
                .marks()
                .iter()
                .filter_map(|mark| Some(mark.as_stroke()?.points[0].1))
                .collect()
        };
        assert_eq!(heights(&marks), [15.]);
        assert!(marks.undo());
        assert_eq!(heights(&marks), [5., 10., 15.]);
        assert!(marks.redo());
        assert_eq!(marks.marks().len(), 1);
        // Erase all, undone and redone.
        assert!(marks.clear());
        assert!(marks.is_empty());
        assert!(!marks.clear());
        assert!(marks.undo());
        assert_eq!(marks.marks().len(), 1);
        assert!(marks.redo());
        assert!(marks.is_empty());
        // Undo walks back through every change to the start.
        while marks.undo() {}
        assert!(marks.is_empty() && !marks.can_undo() && marks.can_redo());
    }

    #[test]
    fn a_stroke_is_touched_within_its_width_and_reach() {
        let stroke = line(Tool::Pen, Rgb(0, 0, 0), 10.);
        // Width 6: 3 either side of y = 10, from x 2 to 18.
        assert!(stroke.touches((10., 12.), 0.));
        assert!(!stroke.touches((10., 15.), 0.));
        assert!(stroke.touches((10., 15.), 3.));
        assert!(!stroke.touches((30., 10.), 3.));
    }

    #[test]
    fn colours_read_back_as_written() {
        let red = Rgb(0xE6, 0x1B, 0x1B);
        assert_eq!(red.to_hex_string(), "#E61B1B");
        assert_eq!(Rgb::from_hex_string("#E61B1B"), Some(red));
        assert_eq!(Rgb::from_hex_string("E61B1B"), None);
        assert_eq!(Rgb::from_hex_string("#E61B1"), None);
        assert_eq!(Rgb::from_hex_string("#GGGGGG"), None);
        // The defaults are in their palettes.
        assert!(PEN_COLORS.contains(&Brush::PEN.color));
        assert!(HIGHLIGHTER_COLORS.contains(&Brush::HIGHLIGHTER.color));
    }

    #[test]
    fn edits_undo_alone_or_in_runs() {
        let mut marks = Marks::default();
        let at = |y| Mark::Stroke(line(Tool::Pen, Rgb(0, 0, 0), y));
        marks.add(at(1.));
        // Two nudges, one undo; a drag after them, its own.
        marks.edit(0, at(2.), Edit::Nudge);
        marks.edit(0, at(3.), Edit::Nudge);
        marks.edit(0, at(4.), Edit::Once);
        assert_eq!(marks.undo_edits(), Some(0));
        assert!(marks.undo());
        assert_eq!(marks.marks()[0], at(3.));
        assert!(marks.undo());
        assert_eq!(marks.marks()[0], at(1.));
        assert_eq!(marks.redo_edits(), Some(0));
        assert!(marks.redo());
        assert_eq!(marks.marks()[0], at(3.));
        // A style run after a nudge run starts its own.
        marks.edit(0, at(5.), Edit::Style);
        marks.edit(0, at(6.), Edit::Style);
        assert!(marks.undo());
        assert_eq!(marks.marks()[0], at(3.));
        // Undo past the add: nothing to edit.
        assert!(marks.undo() && marks.undo());
        assert_eq!(marks.undo_edits(), None);
    }

    #[test]
    fn undo_and_redo_walk_the_strokes() {
        let mut marks = Marks::default();
        let stroke = Mark::Stroke(line(Tool::Pen, Rgb(0, 0, 0), 5.));
        marks.add(stroke.clone());
        marks.add(stroke.clone());
        assert!(marks.undo());
        assert_eq!(marks.marks().len(), 1);
        assert!(marks.redo());
        assert!(!marks.redo());
        assert_eq!(marks.marks().len(), 2);
        marks.undo();
        // A new stroke ends what can be redone.
        marks.add(stroke);
        assert!(!marks.can_redo());
        assert!(marks.undo() && marks.undo());
        assert!(!marks.undo());
    }
}
