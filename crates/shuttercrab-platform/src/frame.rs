//! The dashed border around the area being recorded (PRD §7.7), like the
//! Snipping Tool's: four thin strips the pointer passes through, which
//! never take the keyboard.
//!
//! A side goes just outside the area, so it cannot be in the recording
//! even if the OS fails to exclude it. Where the area meets the edge of
//! its monitor there is no outside, and that side goes just inside; it is
//! shown only if the OS confirms it is excluded from capture.

/// A rectangle in physical, virtual-desktop pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn right(&self) -> i32 {
        self.x + self.width as i32
    }

    fn bottom(&self) -> i32 {
        self.y + self.height as i32
    }

    fn overlaps(&self, other: &Rect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// Bounds with no edge anywhere near: every side of a border around a
/// recorded window goes outside it. The window's own picture is recorded,
/// so the border never is, even over another monitor's edge.
pub const NO_EDGE: Rect = Rect {
    x: -(1 << 24),
    y: -(1 << 24),
    width: 1 << 25,
    height: 1 << 25,
};

/// How the border looks, physical pixels.
#[derive(Clone, Copy, Debug)]
pub struct FrameStyle {
    pub thickness: u32,
    pub dash: u32,
    pub gap: u32,
}

/// Where the border's four strips go around `area` on a monitor with
/// `bounds`: each side outside the area if the monitor has room, otherwise
/// inside it. Returns the strips (top, bottom, left, right) and whether
/// each covers part of the area.
pub fn strips(area: Rect, bounds: Rect, thickness: u32) -> Vec<(Rect, bool)> {
    let t = thickness as i32;
    let left = if area.x - t >= bounds.x {
        area.x - t
    } else {
        area.x
    };
    let top = if area.y - t >= bounds.y {
        area.y - t
    } else {
        area.y
    };
    let right = if area.right() + t <= bounds.right() {
        area.right() + t
    } else {
        area.right()
    };
    let bottom = if area.bottom() + t <= bounds.bottom() {
        area.bottom() + t
    } else {
        area.bottom()
    };
    let (width, height) = ((right - left).max(0) as u32, (bottom - top).max(0) as u32);
    let side = height.saturating_sub(2 * thickness);
    [
        Rect::new(left, top, width, thickness),
        Rect::new(left, bottom - t, width, thickness),
        Rect::new(left, top + t, thickness, side),
        Rect::new(right - t, top + t, thickness, side),
    ]
    .into_iter()
    .filter(|r| r.width > 0 && r.height > 0)
    .map(|r| (r, r.overlaps(&area)))
    .collect()
}

/// The dashes of one strip, premultiplied BGRA: `color` (RGB) along its
/// length, clear in the gaps. The pattern follows desktop coordinates, so
/// the strips' dashes line up at the corners.
pub fn dashes(strip: Rect, style: FrameStyle, color: [u8; 3]) -> Vec<u8> {
    let period = (style.dash + style.gap).max(1) as i32;
    let horizontal = strip.width >= strip.height;
    let mut bgra = Vec::with_capacity((strip.width * strip.height * 4) as usize);
    for y in 0..strip.height as i32 {
        for x in 0..strip.width as i32 {
            let along = if horizontal { strip.x + x } else { strip.y + y };
            let on = along.rem_euclid(period) < style.dash as i32;
            bgra.extend(if on {
                [color[2], color[1], color[0], 255]
            } else {
                [0, 0, 0, 0]
            });
        }
    }
    bgra
}

pub use crate::sys::imp::frame::Frame;

#[cfg(test)]
mod tests {
    use super::*;

    const MONITOR: Rect = Rect {
        x: 0,
        y: 0,
        width: 3840,
        height: 2160,
    };

    #[test]
    fn an_area_with_room_is_framed_outside() {
        let area = Rect::new(100, 200, 800, 600);
        let got = strips(area, MONITOR, 3);
        assert_eq!(
            got,
            [
                (Rect::new(97, 197, 806, 3), false),
                (Rect::new(97, 800, 806, 3), false),
                (Rect::new(97, 200, 3, 600), false),
                (Rect::new(900, 200, 3, 600), false),
            ]
        );
    }

    #[test]
    fn sides_at_the_monitor_edge_go_inside() {
        // Touching the top-left corner of the monitor.
        let area = Rect::new(0, 0, 800, 600);
        let got = strips(area, MONITOR, 3);
        assert_eq!(
            got,
            [
                (Rect::new(0, 0, 803, 3), true),
                (Rect::new(0, 600, 803, 3), false),
                (Rect::new(0, 3, 3, 597), true),
                (Rect::new(800, 3, 3, 597), false),
            ]
        );
        // The whole display: every side inside.
        assert!(
            strips(MONITOR, MONITOR, 3)
                .iter()
                .all(|(_, covers)| *covers)
        );
    }

    #[test]
    fn a_monitor_off_the_origin_keeps_its_own_edges() {
        let bounds = Rect::new(3840, 0, 3840, 2160);
        // At the left edge of the second monitor: the left side goes
        // inside rather than onto the first monitor.
        let got = strips(Rect::new(3840, 100, 400, 300), bounds, 2);
        assert_eq!(got[2], (Rect::new(3840, 100, 2, 300), true));
        assert!(!got[0].1);
    }

    #[test]
    fn dashes_repeat_and_leave_the_gaps_clear() {
        let style = FrameStyle {
            thickness: 1,
            dash: 3,
            gap: 2,
        };
        let row = dashes(Rect::new(0, 0, 10, 1), style, [0xE5, 0x48, 0x4D]);
        let alpha: Vec<u8> = row.chunks(4).map(|p| p[3]).collect();
        assert_eq!(alpha, [255, 255, 255, 0, 0, 255, 255, 255, 0, 0]);
        // BGRA, fully opaque, so no premultiplying changes the colour.
        assert_eq!(&row[..4], [0x4D, 0x48, 0xE5, 255]);
        // A vertical strip follows y; a strip starting mid-period lines up.
        let column = dashes(Rect::new(0, 2, 1, 4), style, [255, 0, 0]);
        let alpha: Vec<u8> = column.chunks(4).map(|p| p[3]).collect();
        assert_eq!(alpha, [255, 0, 0, 255]);
    }
}
