//! Recording where the OS has no backend yet.

use super::unsupported;
use crate::{
    record::{Interruption, Microphone, RecordOptions, RecordingSummary, Source},
    screen::MonitorId,
};
use anyhow::Result;

/// A recorder that cannot start.
pub struct Recorder;

impl Recorder {
    pub fn start(_options: RecordOptions) -> Result<Self> {
        Err(unsupported("Screen recording"))
    }

    pub fn pause(&self) {}

    pub fn resume(&self) {}

    pub fn set_sound(&self, _source: Source, _on: bool) {}

    pub fn set_sound_process(&self, _process: Option<u32>) {}

    pub fn show_cursor(&self, _show: bool) {}

    pub fn window_closed(&self) {}

    pub fn display_gone(&self) {}

    pub fn display_changed(&self) {}

    pub fn ended(&mut self) -> Option<impl std::future::Future<Output = ()> + 'static> {
        None::<std::future::Ready<()>>
    }

    pub fn stop(self) -> Result<RecordingSummary> {
        Err(unsupported("Screen recording"))
    }
}

pub fn attached(_monitor: MonitorId) -> bool {
    false
}

pub fn microphones() -> Result<Vec<Microphone>> {
    Ok(Vec::new())
}

pub(crate) fn interruption(e: &anyhow::Error) -> Interruption {
    Interruption::Failed(format!("{e:#}"))
}
