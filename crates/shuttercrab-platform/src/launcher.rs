//! Shuttercrab's entry in the desktop's app launcher, where the app adds
//! it itself: the desktop finds the app's name and icon for its windows
//! and notifications there. Windows' installer adds the Start menu entry,
//! so there this does nothing.

/// The name Shuttercrab's windows and launcher entry share, so the desktop
/// can tell they belong together.
pub const APP_ID: &str = "shuttercrab";

pub use crate::sys::imp::launcher::register;
