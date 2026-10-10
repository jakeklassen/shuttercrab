//! Screen recording (PRD §13): one monitor or one window, cropped to a
//! region, converted to SDR frame by frame, and encoded to H.264 in an
//! MP4, with the speakers' and a microphone's sound as an AAC track.
//!
//! This module is what the app sees on every OS; the recorder itself is in
//! the OS backend. [`exposure`] keeps HDR exposure steady and [`size`] says
//! what of a monitor can be recorded, for every backend.

pub(crate) mod exposure;
mod size;

pub(crate) use size::HARDWARE_MIN_SIDE;
pub use size::{MIN_SIDE, recordable};

pub use crate::sys::imp::record::{Recorder, attached, microphones};

use crate::screen::{MonitorId, PhysicalRect};
use std::{path::PathBuf, time::Duration};

/// 100-nanosecond units: the recorder's clock.
pub(crate) const TICKS_PER_SECOND: i64 = 10_000_000;

/// What to record.
#[derive(Clone, Debug)]
pub struct RecordOptions {
    pub monitor: MonitorId,
    /// Physical pixels relative to the monitor; `None` records all of it.
    /// Rounded down to even sizes, as NV12 requires.
    pub region: Option<PhysicalRect>,
    /// A window (its `HWND`) to record instead of the monitor: its own
    /// picture, wherever it goes. The video keeps the size the window has
    /// when recording starts; resized, it is fitted inside.
    pub window: Option<isize>,
    /// 30 or 60 (PRD §13.1).
    pub fps: u32,
    pub include_cursor: bool,
    /// Record what the speakers play (off unless asked).
    pub system_sound: bool,
    /// Of what the speakers play, only what this process and those it
    /// started play (a recorded window's app); `None` for all of it.
    pub sound_process: Option<u32>,
    /// Record a microphone (off unless asked): the one named, or Windows'
    /// default.
    pub microphone: bool,
    pub microphone_device: Option<String>,
    /// Give the file a sound track even with every source off, so one can
    /// be switched on mid-recording.
    pub sound_track: bool,
    pub path: PathBuf,
}

impl RecordOptions {
    /// Whether the file gets a sound track.
    pub fn has_sound(&self) -> bool {
        self.sound_track || self.system_sound || self.microphone
    }
}

/// What a finished recording contains.
#[derive(Clone, Debug)]
pub struct RecordingSummary {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Frames written, including the repeat of the last frame at the stop.
    pub frames: u64,
    /// Frames dropped because every encoder texture was busy.
    pub dropped_busy: u64,
    /// Frames skipped because they came faster than the frame rate.
    pub skipped_rate: u64,
    /// The output duration: capture time minus paused time.
    pub duration: Duration,
    /// Time spent paused.
    pub paused: Duration,
    /// Whether Media Foundation chose a hardware encoder.
    pub hardware_encoder: bool,
    /// Whether the file has a sound track.
    pub sound: bool,
    /// Why the recording ended before it was stopped, if it did. What was
    /// recorded until then is still in the file.
    pub interrupted: Option<Interruption>,
    /// Where each kept frame's time went, when it could be measured.
    pub timing: Option<FrameTiming>,
}

/// Why a recording ended by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Interruption {
    /// The recorded display was disconnected or turned off, or went away
    /// while the graphics driver restarted (a driver update).
    DisplayGone,
    /// The recorded window was closed.
    WindowClosed,
    /// The graphics driver was reset or the device removed.
    DeviceLost,
    /// The file could not be written because the disk is full.
    DiskFull,
    /// Anything else, as the error said.
    Failed(String),
}

impl Interruption {
    /// What `e`, an error during recording, means for the user.
    pub fn from_error(e: &anyhow::Error) -> Self {
        crate::sys::imp::record::interruption(e)
    }

    /// Why recording stopped, to finish "Recording stopped: …".
    pub fn describe(&self) -> String {
        match self {
            Self::DisplayGone => {
                "the display was disconnected or turned off, or the graphics driver restarted"
                    .into()
            }
            Self::WindowClosed => "the window was closed".into(),
            Self::DeviceLost => "the graphics driver was reset".into(),
            Self::DiskFull => "the disk is full".into(),
            Self::Failed(_) => "something went wrong (the log has the details)".into(),
        }
    }
}

/// Average time per kept frame in each stage of the recorder, for tuning
/// its cost to a game (issue #15). GPU times come from timestamp queries;
/// CPU times are wall time on the recording thread.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameTiming {
    /// Frames with GPU times (some are lost when the GPU is slow to answer).
    pub gpu_frames: u64,
    /// Copying the recorded region out of the captured frame.
    pub copy_ms: f64,
    /// HDR analysis, tone mapping, and the copy to the converter's input.
    pub convert_ms: f64,
    /// RGBA → NV12 in the video processor.
    pub nv12_ms: f64,
    /// Frames with CPU times.
    pub cpu_frames: u64,
    /// The conversion call, including its wait for the analysis read-back.
    pub cpu_convert_ms: f64,
    /// Handing the frame to the encoder.
    pub cpu_write_ms: f64,
}

impl std::fmt::Display for FrameTiming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "GPU per frame ({} frames): copy {:.2} ms, analysis + tone map {:.2} ms, NV12 {:.2} ms, \
             total {:.2} ms; CPU per frame ({} frames): convert call {:.2} ms, encoder hand-off {:.2} ms",
            self.gpu_frames,
            self.copy_ms,
            self.convert_ms,
            self.nv12_ms,
            self.copy_ms + self.convert_ms + self.nv12_ms,
            self.cpu_frames,
            self.cpu_convert_ms,
            self.cpu_write_ms
        )
    }
}

/// Where a recording's sound comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// What the speakers play.
    System,
    /// A microphone.
    Microphone,
}

/// A microphone Windows knows of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Microphone {
    /// Windows' id for it, to record from it.
    pub id: String,
    /// Its name, as Windows' sound settings show it.
    pub name: String,
}
