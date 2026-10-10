//! Playing a recording back: the OS's media engine opens the file, decodes
//! it and plays its sound itself; the app asks it for each new picture,
//! scaled to the size it shows it at.

pub use crate::sys::imp::play::Player;

/// What the media engine tells the app about the file it plays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerEvent {
    /// The file is open: its length and picture size are known.
    Loaded,
    /// The first picture is ready to show.
    FirstFrame,
    Playing,
    Paused,
    /// A seek finished: the picture at the new time is ready.
    Seeked,
    /// Playing reached the end.
    Ended,
    /// The file cannot be played.
    Failed(String),
}
