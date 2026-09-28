//! Framecut, the application: a tray app that turns a hotkey into a
//! frozen-screen selection and a PNG on the clipboard and on disk.
//!
//! [`selection`] converts the pointer's logical pixels to physical capture
//! pixels, [`overlay`] is the selection view, and [`app`] runs the flow.
#![cfg(windows)]

pub mod app;
pub mod files;
pub mod logging;
pub mod overlay;
pub mod selection;
pub mod settings;
