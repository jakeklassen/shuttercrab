//! The windows a screenshot can target: visible top-level windows, front to
//! back, with the bounds the user sees (PRD §7.3).

/// A window on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowTarget {
    pub id: WindowId,
    /// What the user sees of the window, without the invisible resize
    /// borders: the area a window capture takes.
    pub bounds: PhysicalRect,
    /// The desktop (wallpaper and icons) rather than an application window.
    pub desktop: bool,
    /// The window class, for diagnostics. Never logged with titles.
    pub class: String,
}

/// Visible top-level windows, front to back. Minimised, cloaked (other
/// virtual desktops, suspended apps) and click-through windows (overlays
/// that draw over everything) are left out, and so is Shuttercrab's own
/// capture UI (excluded from capture).
pub use crate::sys::imp::targets::visible_windows;
use shuttercrab_types::{PhysicalRect, WindowId};
