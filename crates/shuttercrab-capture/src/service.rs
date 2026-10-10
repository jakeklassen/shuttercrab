//! The capture API the application uses (PRD §19): a cheap, cloneable
//! [`Capture`] handle to a service thread that owns the OS's capture and
//! GPU objects. Callers await its results; no OS object crosses to them.

pub use crate::sys::imp::service::{Capture, is_device_lost, monitor_under_pointer};
