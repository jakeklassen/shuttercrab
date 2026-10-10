//! The Linux backend. So far: what the system is called, one Shuttercrab
//! per user with requests from a second start, notifications, and helper
//! processes. The rest is still the unsupported backend's.

pub mod instance;
pub mod os;
pub mod platform;
pub mod process;

pub use super::unsupported::{
    console, cursor, drag, frame, hotkey, memory, ocr, open, startup, targets, watch, window,
};
