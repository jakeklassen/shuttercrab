//! Framecut, the application: a background process that turns a hotkey into
//! a frozen-screen selection and a PNG on the clipboard.
//!
//! [`selection`] converts the pointer's logical pixels to physical capture
//! pixels, [`overlay`] is the selection view, and [`app`] runs the flow.
#![cfg(windows)]

pub mod app;
pub mod overlay;
pub mod selection;
