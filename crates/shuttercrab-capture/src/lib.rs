//! Shuttercrab's capture and color pipeline: monitors with their HDR state
//! and SDR white level, one-frame capture, the HDR/WCG to SDR transform,
//! recording and playback. See docs/COLOR_PIPELINE.md.
//!
//! Each public module is the same on every OS. What only an OS can do lives
//! in `sys`, one backend per OS; Windows is the only one so far, and other
//! OSes get one that says they are not supported yet.

pub mod color;
pub mod play;
pub mod png_io;
pub mod raw;
pub mod record;
pub mod screen;
pub mod service;
pub mod shape;
mod sys;

/// The Windows backend's own modules, for the diagnostic tools: capture,
/// display state and the GPU.
#[cfg(windows)]
pub use sys::windows::{capture, display, gpu};

pub use screen::{
    CaptureError, CaptureErrorCode, FrozenFrame, MonitorId, MonitorInfo, PhysicalRect, Screenshot,
    WindowId, cut, cut_shape, encode_png, premultiplied_bgra_to_rgba,
};
pub use service::{Capture, monitor_under_pointer};
