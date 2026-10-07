//! Shuttercrab's capture and color pipeline: monitor enumeration with Advanced
//! Color / HDR state and SDR white level, one-frame capture through
//! Windows.Graphics.Capture in FP16, and the HDR/WCG to SDR transform on the
//! GPU. See docs/COLOR_PIPELINE.md.
#![cfg(windows)]

pub mod capture;
pub mod color;
pub mod display;
pub mod gpu;
pub mod play;
pub mod png_io;
pub mod raw;
pub mod record;
pub mod service;
pub mod shape;

pub use service::{
    Capture, CaptureError, CaptureErrorCode, FrozenFrame, MonitorId, MonitorInfo, PhysicalRect,
    Screenshot, cut, cut_shape, encode_png, monitor_under_pointer, premultiplied_bgra_to_rgba,
};
