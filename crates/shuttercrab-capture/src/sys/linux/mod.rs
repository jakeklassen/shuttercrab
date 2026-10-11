//! The Linux backend. So far: screenshots and recording (picture only)
//! under X11. Playback is still the unsupported backend's.

pub mod record;
pub mod service;

pub use super::unsupported::play;
