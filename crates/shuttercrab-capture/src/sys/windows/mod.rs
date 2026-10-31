//! The Windows backend: Windows.Graphics.Capture and Direct3D 11 for the
//! screen and the colour transform, Media Foundation for recording and
//! playback, and WASAPI for sound.

pub mod capture;
mod clock;
mod com;
pub mod display;
pub mod gpu;
pub mod live_player;
mod opus;
pub mod play;
pub mod record;
pub mod service;

use std::sync::{Mutex, MutexGuard, PoisonError};

/// What `mutex` guards, even if a thread panicked holding it: the threads
/// that share one leave it whole between steps, and a player or sink that
/// gave up on it would stop for good.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
