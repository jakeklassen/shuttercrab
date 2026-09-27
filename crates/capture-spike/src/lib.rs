//! Framecut Milestone 0: capture one monitor through Windows.Graphics.Capture
//! in FP16, convert it to SDR on the GPU, and check the result against an
//! HDR-off reference. See docs/COLOR_PIPELINE.md and docs/TEST_MATRIX.md.
#![cfg(windows)]

pub mod color;
