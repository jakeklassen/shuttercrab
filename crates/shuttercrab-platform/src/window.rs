//! Window behaviour GPUI does not offer, applied to a window by its native
//! handle: placing, covering, hiding and showing it, keeping it out of
//! captures, and the monitor and work area it is on.

pub use crate::sys::imp::window::{
    bring_to_front, client_bounds, cover, exclude_from_capture, fit_client_area, foreground_window,
    hide, include_in_capture, is_on_screen, monitor_of, never_activate, of, outer_bounds, place,
    round_corners, show_normal, sound_process, work_area,
};
