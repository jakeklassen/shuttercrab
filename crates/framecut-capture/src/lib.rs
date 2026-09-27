//! Framecut's capture and color pipeline: monitor enumeration with Advanced
//! Color / HDR state and SDR white level, one-frame capture through
//! Windows.Graphics.Capture in FP16, and the HDR/WCG to SDR transform on the
//! GPU. See docs/COLOR_PIPELINE.md.
#![cfg(windows)]

pub mod capture;
pub mod color;
pub mod display;
pub mod gpu;
pub mod png_io;
pub mod raw;
