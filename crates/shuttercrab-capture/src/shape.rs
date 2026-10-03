//! Freeform screenshots: everything outside a drawn outline made
//! transparent, with anti-aliased edges.
//!
//! The outline is filled by the nonzero winding rule, so a loop that crosses
//! itself still fills solid. Each pixel's coverage is sampled on
//! [`SUBROWS`] rows per pixel, with exact horizontal coverage on each.

/// Sample rows per pixel row.
const SUBROWS: usize = 4;

/// Make the pixels of `rgba` (`width` × `height`, straight alpha) outside
/// `outline` transparent, and those on its edge partly so. `outline` is in
/// the image's own pixel coordinates and is closed automatically.
pub fn mask_outside(rgba: &mut [u8], width: u32, height: u32, outline: &[(f32, f32)]) {
    let (w, h) = (width as usize, height as usize);
    debug_assert_eq!(rgba.len(), w * h * 4);
    let mut coverage = vec![0f32; w];
    let mut crossings: Vec<(f32, i32)> = Vec::new();
    for row in 0..h {
        coverage.fill(0.0);
        for sub in 0..SUBROWS {
            let y = row as f32 + (sub as f32 + 0.5) / SUBROWS as f32;
            crossings.clear();
            for (i, &(x0, y0)) in outline.iter().enumerate() {
                let (x1, y1) = outline[(i + 1) % outline.len()];
                // Half-open in y, so a vertex on the row counts once.
                let (direction, low, high) = if y0 <= y1 {
                    (1, (x0, y0), (x1, y1))
                } else {
                    (-1, (x1, y1), (x0, y0))
                };
                if y >= low.1 && y < high.1 {
                    let t = (y - low.1) / (high.1 - low.1);
                    crossings.push((low.0 + t * (high.0 - low.0), direction));
                }
            }
            crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut winding = 0;
            for pair in crossings.windows(2) {
                winding += pair[0].1;
                if winding != 0 {
                    add_span(&mut coverage, pair[0].0, pair[1].0, 1.0 / SUBROWS as f32);
                }
            }
        }
        for (x, covered) in coverage.iter().enumerate() {
            let alpha = &mut rgba[(row * w + x) * 4 + 3];
            *alpha = (f32::from(*alpha) * covered.clamp(0.0, 1.0)).round() as u8;
        }
    }
}

/// Add `weight` × how much of each pixel the span `start..end` covers.
fn add_span(coverage: &mut [f32], start: f32, end: f32, weight: f32) {
    let start = start.max(0.0);
    let end = end.min(coverage.len() as f32);
    if end <= start {
        return;
    }
    let (first, last) = (
        start.floor() as usize,
        (end.ceil() as usize).min(coverage.len()),
    );
    for (x, covered) in coverage.iter_mut().enumerate().take(last).skip(first) {
        let inside = end.min(x as f32 + 1.0) - start.max(x as f32);
        *covered += inside.max(0.0) * weight;
    }
}

/// The smallest whole-pixel rectangle around `outline`, clamped to a
/// `width` × `height` image: (x, y, width, height), or `None` if nothing of
/// it is inside.
pub fn bounds(outline: &[(f32, f32)], width: u32, height: u32) -> Option<(i32, i32, u32, u32)> {
    let (mut left, mut top) = (f32::INFINITY, f32::INFINITY);
    let (mut right, mut bottom) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for &(x, y) in outline {
        left = left.min(x);
        top = top.min(y);
        right = right.max(x);
        bottom = bottom.max(y);
    }
    let left = left.floor().max(0.0) as i32;
    let top = top.floor().max(0.0) as i32;
    let right = (right.ceil() as i32).min(width as i32);
    let bottom = (bottom.ceil() as i32).min(height as i32);
    (right > left && bottom > top)
        .then(|| (left, top, (right - left) as u32, (bottom - top) as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opaque(width: u32, height: u32) -> Vec<u8> {
        vec![200; (width * height * 4) as usize]
    }

    fn alpha(rgba: &[u8], width: u32, x: u32, y: u32) -> u8 {
        rgba[((y * width + x) * 4 + 3) as usize]
    }

    #[test]
    fn a_square_outline_keeps_its_inside_and_clears_its_outside() {
        let mut rgba = opaque(10, 10);
        mask_outside(
            &mut rgba,
            10,
            10,
            &[(2.0, 2.0), (8.0, 2.0), (8.0, 8.0), (2.0, 8.0)],
        );
        assert_eq!(alpha(&rgba, 10, 5, 5), 200);
        assert_eq!(alpha(&rgba, 10, 2, 2), 200);
        assert_eq!(alpha(&rgba, 10, 1, 5), 0);
        assert_eq!(alpha(&rgba, 10, 8, 5), 0);
        assert_eq!(alpha(&rgba, 10, 5, 9), 0);
        // Colour is left alone: straight alpha.
        assert_eq!(rgba[((5 * 10 + 1) * 4) as usize], 200);
    }

    #[test]
    fn an_edge_through_a_pixel_covers_it_partly() {
        let mut rgba = opaque(4, 4);
        mask_outside(
            &mut rgba,
            4,
            4,
            &[(0.0, 0.0), (2.5, 0.0), (2.5, 4.0), (0.0, 4.0)],
        );
        assert_eq!(alpha(&rgba, 4, 1, 1), 200);
        assert_eq!(alpha(&rgba, 4, 2, 1), 100);
        assert_eq!(alpha(&rgba, 4, 3, 1), 0);
    }

    #[test]
    fn a_loop_that_crosses_itself_fills_solid() {
        // Round the square twice: winding 2 inside, still filled.
        let square = [(1.0, 1.0), (9.0, 1.0), (9.0, 9.0), (1.0, 9.0)];
        let twice: Vec<_> = square.iter().chain(square.iter()).copied().collect();
        let mut rgba = opaque(10, 10);
        mask_outside(&mut rgba, 10, 10, &twice);
        assert_eq!(alpha(&rgba, 10, 5, 5), 200);
    }

    #[test]
    fn bounds_are_whole_pixels_inside_the_image() {
        let outline = [(-3.0, 2.4), (7.6, 1.2), (4.0, 12.0)];
        assert_eq!(bounds(&outline, 10, 10), Some((0, 1, 8, 9)));
        assert_eq!(bounds(&[(20.0, 20.0), (30.0, 30.0)], 10, 10), None);
    }
}
