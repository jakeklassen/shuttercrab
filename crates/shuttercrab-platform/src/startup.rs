//! Launching Shuttercrab when the user signs in (PRD §26).

/// On the command line of a start at sign-in: start in the tray, without
/// opening the window.
pub const BACKGROUND_FLAG: &str = "--background";

pub use crate::sys::imp::startup::{add_background_flag, launch_at_startup, set_launch_at_startup};
