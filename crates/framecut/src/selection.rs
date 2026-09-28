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

/// A window on the frozen monitor: physical pixels relative to the
/// monitor's top-left. It may extend past the monitor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenWindow {
    pub hwnd: isize,
    pub bounds: PhysicalRect,
}

fn contains(r: &PhysicalRect, x: i32, y: i32) -> bool {
    x >= r.x && y >= r.y && x - r.x < r.width as i32 && y - r.y < r.height as i32
}

/// The front-most window at physical point (`x`, `y`); `windows` are front
/// to back.
pub fn window_at(windows: &[ScreenWindow], x: i32, y: i32) -> Option<&ScreenWindow> {
    windows.iter().find(|w| contains(&w.bounds, x, y))
}

/// Move a pointer position (physical pixels, relative to the monitor) onto
/// a nearby window edge or monitor edge, each axis separately, when one is
/// within `threshold` pixels (PRD §7.2). Only edges the user can see count:
/// an edge covered by a window in front, or far along from the pointer,
/// does not pull.
pub fn snap(
    windows: &[ScreenWindow],
    x: f32,
    y: f32,
    threshold: f32,
    width: u32,
    height: u32,
) -> (f32, f32) {
    let nearest = |value: f32, edges: &mut dyn Iterator<Item = i32>| {
        edges
            .map(|e| (e, (e as f32 - value).abs()))
            .filter(|&(_, d)| d <= threshold)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map_or(value, |(e, _)| e as f32)
    };
    // The window must be the front-most one on its own edge, level with the
    // pointer (or as close as its extent allows, within the threshold).
    let visible_edge = |w: &ScreenWindow, edge_x: i32, edge_y: i32| {
        window_at(windows, edge_x, edge_y).is_some_and(|front| front.hwnd == w.hwnd)
    };
    let (px, py) = (x.round() as i32, y.round() as i32);
    let mut vertical = windows.iter().flat_map(|w| {
        let b = w.bounds;
        let level = py.clamp(b.y, b.y + b.height as i32 - 1);
        let near = (level - py).abs() as f32 <= threshold;
        [(b.x, b.x), (b.x + b.width as i32, b.x + b.width as i32 - 1)]
            .into_iter()
            .filter(move |&(edge, _)| near && (0..=width as i32).contains(&edge))
            .filter(move |&(_, inside)| visible_edge(w, inside, level))
            .map(|(edge, _)| edge)
    });
    let mut horizontal = windows.iter().flat_map(|w| {
        let b = w.bounds;
        let level = px.clamp(b.x, b.x + b.width as i32 - 1);
        let near = (level - px).abs() as f32 <= threshold;
        [
            (b.y, b.y),
            (b.y + b.height as i32, b.y + b.height as i32 - 1),
        ]
        .into_iter()
        .filter(move |&(edge, _)| near && (0..=height as i32).contains(&edge))
        .filter(move |&(_, inside)| visible_edge(w, level, inside))
        .map(|(edge, _)| edge)
    });
    let sx = nearest(x, &mut [0, width as i32].into_iter().chain(&mut vertical));
    let sy = nearest(
        y,
        &mut [0, height as i32].into_iter().chain(&mut horizontal),
    );
    (sx, sy)
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

    fn win(hwnd: isize, x: i32, y: i32, width: u32, height: u32) -> ScreenWindow {
        ScreenWindow {
            hwnd,
            bounds: PhysicalRect::new(x, y, width, height),
        }
    }

    #[test]
    fn the_front_most_window_is_hit() {
        let windows = [win(1, 50, 50, 100, 100), win(2, 0, 0, 400, 300)];
        assert_eq!(window_at(&windows, 60, 60).map(|w| w.hwnd), Some(1));
        assert_eq!(window_at(&windows, 10, 10).map(|w| w.hwnd), Some(2));
        // Right and bottom edges are exclusive.
        assert_eq!(window_at(&windows, 150, 60).map(|w| w.hwnd), Some(2));
        assert_eq!(window_at(&windows, 500, 10), None);
    }

    #[test]
    fn snaps_to_window_edges_near_the_pointer() {
        let windows = [win(1, 100, 100, 200, 100)];
        let snap = |x, y| snap(&windows, x, y, 8.0, 1000, 800);
        assert_eq!(snap(104.0, 150.0), (100.0, 150.0));
        assert_eq!(snap(295.5, 150.0), (300.0, 150.0));
        assert_eq!(snap(150.0, 93.0), (150.0, 100.0));
        assert_eq!(snap(150.0, 205.0), (150.0, 200.0));
        // Both axes at a corner.
        assert_eq!(snap(97.0, 203.0), (100.0, 200.0));
        // Too far from any edge: unchanged.
        assert_eq!(snap(120.0, 150.0), (120.0, 150.0));
    }

    #[test]
    fn edges_far_along_or_hidden_do_not_snap() {
        // The pointer is level with neither end of the window's left edge.
        let windows = [win(1, 100, 100, 200, 100)];
        assert_eq!(snap(&windows, 104.0, 400.0, 8.0, 1000, 800), (104.0, 400.0));
        // Window 2 in front covers window 1's left edge at this height.
        let windows = [win(2, 50, 120, 100, 50), win(1, 100, 100, 200, 100)];
        assert_eq!(snap(&windows, 103.0, 140.0, 8.0, 1000, 800), (103.0, 140.0));
        // Above window 2, window 1's left edge is visible again.
        assert_eq!(snap(&windows, 103.0, 110.0, 8.0, 1000, 800), (100.0, 110.0));
        // And window 2's own right edge pulls.
        assert_eq!(snap(&windows, 146.0, 140.0, 8.0, 1000, 800), (150.0, 140.0));
    }

    #[test]
    fn snaps_to_the_monitor_edges() {
        assert_eq!(snap(&[], 5.0, 795.0, 8.0, 1000, 800), (0.0, 800.0));
        assert_eq!(snap(&[], 500.0, 400.0, 8.0, 1000, 800), (500.0, 400.0));
    }
}
