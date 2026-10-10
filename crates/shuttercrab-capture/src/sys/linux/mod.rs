//! The Linux backend. So far: screenshots under X11. Recording and playback
//! are still the unsupported backend's.

pub mod service;

pub use super::unsupported::{play, record};
