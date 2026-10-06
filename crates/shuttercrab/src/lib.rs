//! Shuttercrab, the application: a tray app that turns a hotkey into a
//! frozen-screen selection and a PNG on the clipboard and on disk, or into
//! an MP4 recording of an area.
//!
//! [`selection`] converts the pointer's logical pixels to physical capture
//! pixels, [`overlay`] is the selection view, and [`app`] runs the flow.
#![cfg(windows)]

pub mod app;
pub mod capture_bar;
pub mod capture_choice;
pub mod choice_menu;
pub mod countdown;
pub mod cursors;
pub mod files;
pub mod heap;
pub mod icons;
pub mod logging;
pub mod main_window;
pub mod markup;
pub mod overlay;
pub mod palette;
pub mod pixels;
pub mod popup;
pub mod record_bar;
pub mod recorder_process;
pub mod recording;
pub mod selection;
pub mod settings;
pub mod settings_window;
pub mod shot_view;
pub mod thumbnail;
pub mod update;
