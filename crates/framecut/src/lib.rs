//! Framecut, the application: a tray app that turns a hotkey into a
//! frozen-screen selection and a PNG on the clipboard and on disk, or into
//! an MP4 recording of an area.
//!
//! [`selection`] converts the pointer's logical pixels to physical capture
//! pixels, [`overlay`] is the selection view, and [`app`] runs the flow.
#![cfg(windows)]

pub mod app;
pub mod capture_bar;
pub mod countdown;
pub mod files;
pub mod heap;
pub mod icons;
pub mod logging;
pub mod overlay;
pub mod popup;
pub mod record_bar;
pub mod recording;
pub mod selection;
pub mod settings;
pub mod settings_window;
pub mod thumbnail;
