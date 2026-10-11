//! The Linux backend. So far: what the system is called, one Shuttercrab
//! per user with requests from a second start, the tray icon, notifications,
//! the clipboard, helper processes, memory figures, and under X11 global
//! hotkeys, window placement and the windows on screen. The rest is still
//! the unsupported backend's.

pub mod clipboard;
pub mod hotkey;
pub mod instance;
pub mod launcher;
pub mod memory;
pub mod os;
pub mod platform;
pub mod process;
pub mod targets;
pub mod tray;
pub mod window;

pub use super::unsupported::{console, cursor, drag, frame, ocr, open, startup, watch};
