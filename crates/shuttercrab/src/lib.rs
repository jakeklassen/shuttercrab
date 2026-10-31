//! Shuttercrab, the application: a tray app that turns a hotkey into a
//! frozen-screen selection and a PNG on the clipboard and on disk, or into
//! an MP4 recording of an area.
//!
//! [`selection`] converts the pointer's logical pixels to physical capture
//! pixels, [`ui::overlay`] is the selection view, and [`app`] runs the flow.

pub mod app;
pub mod capture_choice;
pub mod files;
pub mod generation;
pub mod heap;
pub mod helpers;
pub mod hotkeys;
pub mod logging;
pub mod markup;
pub mod pixels;
pub mod playback;
pub mod recording;
pub mod selection;
pub mod settings;
pub mod shot_view;
pub mod text;
pub mod ui;
pub mod update;

/// A folder for the unit tests' files: `tmp` in the target folder, which
/// `cargo clean` takes too. Cargo names it only for integration tests
/// (`CARGO_TARGET_TMPDIR`), so it is found from the test binary, in
/// `target/<profile>/deps`.
#[cfg(test)]
fn test_folder() -> std::path::PathBuf {
    let binary = std::env::current_exe().unwrap();
    let folder = binary.ancestors().nth(3).unwrap().join("tmp");
    std::fs::create_dir_all(&folder).unwrap();
    folder
}
