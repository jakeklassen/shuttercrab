//! Selection geometry: GPUI reports the pointer in logical window pixels;
//! capture regions are physical pixels (PRD §20). The conversion happens
//! here, at the UI/capture boundary, and nowhere else.

use framecut_capture::PhysicalRect;
use gpui_kit::{Bounds, Pixels, Point, point, px, size};

/// A drag in progress or finished, in logical window pixels: where the
/// pointer went down, and where it is now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drag {
    pub start: Point<Pixels>,
    pub end: Point<Pixels>,
}

impl Drag {
    pub fn at(position: Point<Pixels>) -> Self {
        Self {
            start: position,
            end: position,
        }
    }

    /// The dragged rectangle in logical pixels, whichever way it was dragged.
    pub fn logical(&self) -> Bounds<Pixels> {
        let (x0, x1) = min_max(self.start.x, self.end.x);
        let (y0, y1) = min_max(self.start.y, self.end.y);
        Bounds {
            origin: point(x0, y0),
            size: size(x1 - x0, y1 - y0),
        }
    }

    /// The dragged rectangle in physical pixels of a frame `width` × `height`
    /// shown at `scale` physical pixels per logical pixel. Each corner snaps
    /// to the nearest physical pixel boundary, and the result is clamped to
    /// the frame, so a drag that leaves the monitor stops at its edge.
    pub fn physical(&self, scale: f32, width: u32, height: u32) -> PhysicalRect {
        let snap =
            |v: Pixels, limit: u32| ((f32::from(v) * scale).round().max(0.0) as u32).min(limit);
        let b = self.logical();
        let x0 = snap(b.origin.x, width);
        let y0 = snap(b.origin.y, height);
        let x1 = snap(b.origin.x + b.size.width, width);
        let y1 = snap(b.origin.y + b.size.height, height);
        PhysicalRect::new(x0 as i32, y0 as i32, x1 - x0, y1 - y0)
    }
}

/// The logical rectangle a physical rectangle covers, for drawing it.
pub fn to_logical(rect: PhysicalRect, scale: f32) -> Bounds<Pixels> {
    Bounds {
        origin: point(px(rect.x as f32 / scale), px(rect.y as f32 / scale)),
        size: size(
            px(rect.width as f32 / scale),
            px(rect.height as f32 / scale),
        ),
    }
}

/// What the live dimension readout says: physical pixels, width × height.
pub fn dimensions(rect: PhysicalRect) -> String {
    format!("{} × {}", rect.width, rect.height)
}

fn min_max(a: Pixels, b: Pixels) -> (Pixels, Pixels) {
    if a <= b { (a, b) } else { (b, a) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drag(x0: f32, y0: f32, x1: f32, y1: f32) -> Drag {
        Drag {
            start: point(px(x0), px(y0)),
            end: point(px(x1), px(y1)),
        }
    }

    #[test]
    fn converts_logical_drags_to_physical_pixels() {
        // 150%: a 100×50 logical drag is 150×75 physical.
        let rect = drag(10.0, 20.0, 110.0, 70.0).physical(1.5, 3840, 2160);
        assert_eq!(rect, PhysicalRect::new(15, 30, 150, 75));
        assert_eq!(dimensions(rect), "150 × 75");
    }

    #[test]
    fn any_drag_direction_gives_the_same_rectangle() {
        let forward = drag(10.0, 20.0, 110.0, 70.0).physical(1.25, 3000, 2000);
        for d in [
            drag(110.0, 70.0, 10.0, 20.0),
            drag(10.0, 70.0, 110.0, 20.0),
            drag(110.0, 20.0, 10.0, 70.0),
        ] {
            assert_eq!(d.physical(1.25, 3000, 2000), forward);
        }
    }

    #[test]
    fn corners_snap_to_the_nearest_physical_pixel() {
        // At 175%, logical 1 = physical 1.75, which rounds to 2.
        assert_eq!(
            drag(1.0, 1.0, 3.0, 3.0).physical(1.75, 100, 100),
            PhysicalRect::new(2, 2, 3, 3)
        );
        // Pointer positions are physical pixels over the scale, so they come
        // back exactly: physical 7 at 175% is logical 4.
        let p = 7.0 / 1.75;
        assert_eq!(
            drag(p, p, p * 2.0, p * 2.0).physical(1.75, 100, 100),
            PhysicalRect::new(7, 7, 7, 7)
        );
    }

    #[test]
    fn drags_are_clamped_to_the_monitor() {
        assert_eq!(
            drag(-20.0, -5.0, 50.0, 40.0).physical(2.0, 80, 60),
            PhysicalRect::new(0, 0, 80, 60)
        );
        assert_eq!(
            drag(30.0, 20.0, 999.0, 999.0).physical(1.0, 100, 50),
            PhysicalRect::new(30, 20, 70, 30)
        );
    }

    #[test]
    fn a_click_without_a_drag_is_empty() {
        assert!(
            Drag::at(point(px(12.0), px(34.0)))
                .physical(1.5, 3840, 2160)
                .is_empty()
        );
    }

    #[test]
    fn physical_rectangles_draw_where_they_were_dragged() {
        let b = to_logical(PhysicalRect::new(15, 30, 150, 75), 1.5);
        assert_eq!((f32::from(b.origin.x), f32::from(b.origin.y)), (10.0, 20.0));
        assert_eq!(
            (f32::from(b.size.width), f32::from(b.size.height)),
            (100.0, 50.0)
        );
    }
}
