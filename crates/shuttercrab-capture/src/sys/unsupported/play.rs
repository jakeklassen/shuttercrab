//! Playback where the OS has no backend yet.

use super::unsupported;
use crate::play::PlayerEvent;
use anyhow::Result;
use std::{path::Path, time::Duration};

/// A player that cannot open anything.
pub struct Player;

impl Player {
    pub fn open(
        _path: &Path,
        _listener: impl Fn(PlayerEvent) + Send + Sync + 'static,
    ) -> Result<Self> {
        Err(unsupported("Playing a recording"))
    }

    pub fn size(&self) -> Option<(u32, u32)> {
        None
    }

    pub fn duration(&self) -> Duration {
        Duration::ZERO
    }

    pub fn position(&self) -> Duration {
        Duration::ZERO
    }

    pub fn play(&self) -> Result<()> {
        Err(unsupported("Playing a recording"))
    }

    pub fn pause(&self) -> Result<()> {
        Ok(())
    }

    pub fn is_paused(&self) -> bool {
        true
    }

    pub fn is_ended(&self) -> bool {
        false
    }

    pub fn seek(&self, _at: Duration) -> Result<()> {
        Ok(())
    }

    pub fn set_volume(&self, _volume: f64) -> Result<()> {
        Ok(())
    }

    pub fn set_muted(&self, _muted: bool) -> Result<()> {
        Ok(())
    }

    pub fn wait_for_refresh(&mut self) -> Result<()> {
        Err(unsupported("Playing a recording"))
    }

    pub fn next_frame(&mut self, _width: u32, _height: u32) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    pub fn frame(&mut self, _width: u32, _height: u32) -> Result<Vec<u8>> {
        Err(unsupported("Playing a recording"))
    }
}
