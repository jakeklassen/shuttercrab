//! Shuttercrab Milestone 0: capture one monitor through Windows.Graphics.Capture
//! in FP16, convert it to SDR on the GPU, and check the result against an
//! HDR-off reference. See docs/COLOR_PIPELINE.md and docs/TESTING.md.
//!
//! The pipeline itself lives in `shuttercrab-capture`; this crate adds the
//! measurement tools and the Milestone 0 gate.
#![cfg(windows)]

pub use shuttercrab_capture::{capture, color, display, gpu};

pub mod analysis;
pub mod fixture;
pub mod gate;
pub mod png_io;
pub mod raw;
pub mod snapshot;

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
