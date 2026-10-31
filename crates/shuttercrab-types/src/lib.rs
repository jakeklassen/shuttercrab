//! What Shuttercrab's crates pass between them, the same on every OS: a
//! window and a monitor on screen, and a rectangle of physical pixels.
//!
//! Only an OS backend makes a [`WindowId`] or [`MonitorId`] from its own
//! handle or turns one back; the app only compares them and passes them
//! on. They name what is on screen now, so they are never saved.

/// A window on screen, as the OS knows it: on Windows, its `HWND`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowId(u64);

impl WindowId {
    /// The window an OS backend knows by `raw`.
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The OS's own value, for its backend and for passing to the recorder
    /// process.
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// A monitor, as the OS knows it: on Windows, its `HMONITOR`, which GPUI
/// also uses as its display id there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MonitorId(u64);

impl MonitorId {
    /// The monitor an OS backend knows by `raw`.
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The OS's own value, for its backend and for passing to the recorder
    /// process.
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// A rectangle in physical pixels. Monitor bounds and windows are in
/// virtual-desktop coordinates; a screenshot or recording region is
/// relative to its monitor's top-left.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PhysicalRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl PhysicalRect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// One past its right edge.
    pub fn right(&self) -> i32 {
        self.x + self.width as i32
    }

    /// One past its bottom edge.
    pub fn bottom(&self) -> i32 {
        self.y + self.height as i32
    }

    /// Whether the pixel at `(x, y)` is inside.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    /// The part inside `other`, if any.
    pub fn intersect(&self, other: &PhysicalRect) -> Option<PhysicalRect> {
        let (x0, y0) = (self.x.max(other.x), self.y.max(other.y));
        let (x1, y1) = (
            self.right().min(other.right()),
            self.bottom().min(other.bottom()),
        );
        (x1 > x0 && y1 > y0).then(|| PhysicalRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
    }

    /// Whether the two share any pixel.
    pub fn overlaps(&self, other: &PhysicalRect) -> bool {
        self.intersect(other).is_some()
    }

    /// The part of `self` inside `0..width` × `0..height`.
    pub fn clamp_to(&self, width: u32, height: u32) -> PhysicalRect {
        let x0 = self.x.clamp(0, width as i32);
        let y0 = self.y.clamp(0, height as i32);
        let x1 = self.right().clamp(0, width as i32);
        let y1 = self.bottom().clamp(0, height as i32);
        PhysicalRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersects_and_contains() {
        let a = PhysicalRect::new(0, 0, 10, 10);
        let b = PhysicalRect::new(5, -5, 10, 10);
        assert_eq!(a.intersect(&b), Some(PhysicalRect::new(5, 0, 5, 5)));
        assert_eq!(a.intersect(&PhysicalRect::new(10, 0, 5, 5)), None);
        assert!(a.contains(0, 0) && a.contains(9, 9));
        assert!(!a.contains(10, 5) && !a.contains(-1, 5));
    }

    #[test]
    fn rectangles_overlap_only_when_they_share_a_pixel() {
        let a = PhysicalRect::new(0, 0, 10, 10);
        assert!(a.overlaps(&PhysicalRect::new(9, 9, 5, 5)));
        assert!(
            !a.overlaps(&PhysicalRect::new(10, 0, 5, 5)),
            "touching is not overlapping"
        );
        assert_eq!(
            PhysicalRect::new(-5, 8, 10, 10).clamp_to(10, 10),
            PhysicalRect::new(0, 8, 5, 2)
        );
    }
}
