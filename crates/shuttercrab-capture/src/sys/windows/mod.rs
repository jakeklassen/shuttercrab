//! The Windows backend: Windows.Graphics.Capture and Direct3D 11 for the
//! screen and the colour transform, Media Foundation for recording and
//! playback, and WASAPI for sound.

pub mod capture;
pub mod display;
pub mod gpu;
pub mod play;
pub mod record;
pub mod service;
