//! The windows a screenshot can target: visible top-level windows, front to
//! back, with the bounds the user sees (PRD §7.3).

/// A rectangle in physical virtual-desktop pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Bounds {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x
            && y >= self.y
            && (x - self.x) < self.width as i32
            && (y - self.y) < self.height as i32
    }

    pub fn right(&self) -> i32 {
        self.x + self.width as i32
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.height as i32
    }

    /// The part inside `other`, if any.
    pub fn intersect(&self, other: &Bounds) -> Option<Bounds> {
        let (x0, y0) = (self.x.max(other.x), self.y.max(other.y));
        let (x1, y1) = (
            self.right().min(other.right()),
            self.bottom().min(other.bottom()),
        );
        (x1 > x0 && y1 > y0).then(|| Bounds {
            x: x0,
            y: y0,
            width: (x1 - x0) as u32,
            height: (y1 - y0) as u32,
        })
    }
}

/// A window on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowTarget {
    pub hwnd: isize,
    /// What the user sees of the window, without the invisible resize
    /// borders: the area a window capture takes.
    pub bounds: Bounds,
    /// The desktop (wallpaper and icons) rather than an application window.
    pub desktop: bool,
    /// The window class, for diagnostics. Never logged with titles.
    pub class: String,
}

/// Visible top-level windows, front to back. Minimised, cloaked (other
pub use crate::sys::imp::targets::visible_windows;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersects_and_contains() {
        let a = Bounds {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        let b = Bounds {
            x: 5,
            y: -5,
            width: 10,
            height: 10,
        };
        assert_eq!(
            a.intersect(&b),
            Some(Bounds {
                x: 5,
                y: 0,
                width: 5,
                height: 5
            })
        );
        assert_eq!(
            a.intersect(&Bounds {
                x: 10,
                y: 0,
                width: 5,
                height: 5
            }),
            None
        );
        assert!(a.contains(0, 0) && a.contains(9, 9));
        assert!(!a.contains(10, 5) && !a.contains(-1, 5));
    }
}
