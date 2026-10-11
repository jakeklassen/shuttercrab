//! The Linux backend. So far: what the system is called, one Shuttercrab
//! per user with requests from a second start, notifications, helper
//! processes, memory figures, and window placement and the windows on
//! screen under X11. The rest is still the unsupported backend's.

pub mod instance;
pub mod launcher;
pub mod memory;
pub mod os;
pub mod platform;
pub mod process;
pub mod targets;
pub mod window;

pub use super::unsupported::{console, cursor, drag, frame, hotkey, ocr, open, startup, watch};
