//! Framecut Milestone 0: capture one monitor through Windows.Graphics.Capture
//! in FP16, convert it to SDR on the GPU, and check the result against an
//! HDR-off reference. See docs/COLOR_PIPELINE.md and docs/TEST_MATRIX.md.
//!
//! The pipeline itself lives in `framecut-capture`; this crate adds the
//! measurement tools and the Milestone 0 gate.
#![cfg(windows)]

pub use framecut_capture::{capture, color, display, gpu, png_io, raw};

pub mod analysis;
pub mod fixture;
pub mod gate;
pub mod snapshot;
