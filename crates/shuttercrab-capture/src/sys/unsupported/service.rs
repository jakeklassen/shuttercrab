//! Capture where the OS has no backend yet.

use crate::screen::{
    CaptureError, FrozenFrame, MonitorId, MonitorInfo, Result, Screenshot, WindowId,
};
use std::future::Future;

/// A capture service that has nothing to capture with.
#[derive(Clone)]
pub struct Capture;

impl Capture {
    /// Starts, so the app can; every request fails.
    pub fn start() -> Result<Self> {
        Ok(Self)
    }

    pub fn warm_up(&self) {}

    pub fn list_monitors(&self) -> impl Future<Output = Result<Vec<MonitorInfo>>> + use<> {
        std::future::ready(Err(CaptureError::unsupported("Screen capture")))
    }

    pub fn freeze_monitor(
        &self,
        _monitor: MonitorId,
        _include_cursor: bool,
    ) -> impl Future<Output = Result<FrozenFrame>> + use<> {
        std::future::ready(Err(CaptureError::unsupported("Screen capture")))
    }

    pub fn capture_window(
        &self,
        _window: WindowId,
        _include_cursor: bool,
    ) -> impl Future<Output = Result<Screenshot>> + use<> {
        std::future::ready(Err(CaptureError::unsupported("Window capture")))
    }
}

pub fn monitor_under_pointer() -> Option<MonitorId> {
    None
}

pub fn is_device_lost(_e: &anyhow::Error) -> bool {
    false
}
