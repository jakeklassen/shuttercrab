//! Screen recording (PRD §13; Milestone 3 spike): one monitor, cropped to a
//! region on the GPU, converted to SDR frame by frame, turned into NV12 by
//! the Direct3D video processor (BT.709), and encoded to H.264 in an MP4 by
//! Media Foundation, hardware accelerated where available.
//!
//! Frames never leave the GPU. Encoder input textures come from a small
//! fixed pool; a frame that arrives while all of them are still with the
//! encoder is dropped and counted, so no queue can grow without bound.
//!
//! The output timeline is Windows' capture time: paused time is removed,
//! frames faster than the frame rate are skipped, and a still screen (which
//! delivers no frames) keeps its duration because the last frame is
//! repeated at the stop time.
//!
//! With sound, what the speakers play is captured too ([`sound`]) and
//! written beside the picture as an AAC track, on the same clock.
//!
//! The pieces: [`session`] runs one recording (capture, conversion, the
//! frame loop), [`encoder`] turns frames into NV12 and an MP4, [`exposure`]
//! keeps HDR exposure steady, [`sound`] captures the sound, and [`timing`]
//! measures each stage's cost.

mod encoder;
mod exposure;
mod session;
mod sound;
mod timing;

pub use sound::{Microphone, Source, microphones};
pub use timing::FrameTiming;

use crate::service::{MonitorId, PhysicalRect, hmonitor};
use anyhow::{Context, Result, bail};
use session::Session;
use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, Sender, channel},
    thread::JoinHandle,
    time::Duration,
};
use windows::Win32::{
    Media::MediaFoundation::*,
    System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
    System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize},
    UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
};

/// What to record.
#[derive(Clone, Debug)]
pub struct RecordOptions {
    pub monitor: MonitorId,
    /// Physical pixels relative to the monitor; `None` records all of it.
    /// Rounded down to even sizes, as NV12 requires.
    pub region: Option<PhysicalRect>,
    /// 30 or 60 (PRD §13.1).
    pub fps: u32,
    pub include_cursor: bool,
    /// Record what the speakers play (off unless asked).
    pub system_sound: bool,
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
    /// The recorded display was disconnected or turned off.
    DisplayGone,
    /// The graphics driver was reset or the device removed.
    DeviceLost,
    /// The file could not be written because the disk is full.
    DiskFull,
    /// Anything else, as the error said.
    Failed(String),
}

/// ERROR_HANDLE_DISK_FULL and ERROR_DISK_FULL, as HRESULTs.
const DISK_FULL: [u32; 2] = [0x8007_0027, 0x8007_0070];

impl Interruption {
    /// What `e`, an error during recording, means for the user.
    pub fn from_error(e: &anyhow::Error) -> Self {
        let codes: Vec<u32> = e
            .chain()
            .filter_map(|cause| cause.downcast_ref::<windows::core::Error>())
            .map(|e| e.code().0 as u32)
            .collect();
        let text = format!("{e:#}").to_ascii_uppercase();
        let any = |list: &[u32]| {
            list.iter()
                .any(|hr| codes.contains(hr) || text.contains(&format!("0X{hr:08X}")))
        };
        if any(&crate::service::DEVICE_LOST) {
            Self::DeviceLost
        } else if any(&DISK_FULL) {
            Self::DiskFull
        } else {
            Self::Failed(format!("{e:#}"))
        }
    }

    /// Why recording stopped, to finish "Recording stopped: …".
    pub fn describe(&self) -> String {
        match self {
            Self::DisplayGone => "the display was disconnected or turned off".into(),
            Self::DeviceLost => "the graphics driver was reset".into(),
            Self::DiskFull => "the disk is full".into(),
            Self::Failed(_) => "something went wrong (the log has the details)".into(),
        }
    }
}

enum Event {
    Frame,
    Pause,
    Resume,
    Stop,
    /// The recorded display went away: Windows closed the capture, or the
    /// app heard that the display is no longer attached.
    DisplayGone,
    /// The displays changed (HDR switched on or off, say): read the
    /// recorded display's white level again.
    DisplayChanged,
    /// Sound from a source.
    Sound(sound::Packet),
    /// Switch a source on or off.
    SetSound(Source, bool),
}

/// A recording in progress, on its own thread.
pub struct Recorder {
    events: Sender<Event>,
    thread: Option<JoinHandle<Result<RecordingSummary>>>,
    /// Resolves when the thread ends, however it ends.
    ended: Option<futures::channel::oneshot::Receiver<()>>,
}

impl Recorder {
    /// Start recording. Returns once the capture and the encoder are running.
    pub fn start(options: RecordOptions) -> Result<Self> {
        let (events, inbox) = channel();
        let (ready, started) = channel::<Result<()>>();
        // Dropped when the thread ends, which resolves `ended`.
        let (ending, ended) = futures::channel::oneshot::channel::<()>();
        let frames = events.clone();
        let thread = std::thread::Builder::new()
            .name("shuttercrab-record".into())
            .spawn(move || {
                let _ending = ending;
                record(options, frames, inbox, ready)
            })
            .context("could not start the recording thread")?;
        match started.recv() {
            Ok(Ok(())) => Ok(Self {
                events,
                thread: Some(thread),
                ended: Some(ended),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => match thread.join() {
                Ok(Err(e)) => Err(e),
                _ => bail!("the recording thread stopped"),
            },
        }
    }

    pub fn pause(&self) {
        let _ = self.events.send(Event::Pause);
    }

    pub fn resume(&self) {
        let _ = self.events.send(Event::Resume);
    }

    /// Switch `source` on or off mid-recording. The file must have a
    /// sound track ([`RecordOptions::has_sound`]).
    pub fn set_sound(&self, source: Source, on: bool) {
        let _ = self.events.send(Event::SetSound(source, on));
    }

    /// The recorded display is gone: end the recording, keeping what was
    /// recorded, as if Windows had closed the capture.
    pub fn display_gone(&self) {
        let _ = self.events.send(Event::DisplayGone);
    }

    /// The displays changed and the recorded one is still attached: carry
    /// on with its white level as it is now, so a recording stays exposed
    /// right when HDR is switched on or off.
    pub fn display_changed(&self) {
        let _ = self.events.send(Event::DisplayChanged);
    }

    /// A future that resolves when the recording ends, by [`Recorder::stop`]
    /// or by itself (the display went away, the device was lost, the disk
    /// filled up). [`Recorder::stop`] then returns at once with what was
    /// saved and why it ended. `None` after the first call.
    pub fn ended(&mut self) -> Option<impl std::future::Future<Output = ()> + 'static> {
        let ended = self.ended.take()?;
        Some(async move {
            let _ = ended.await;
        })
    }

    /// Stop, finish the file, and report what was recorded.
    pub fn stop(mut self) -> Result<RecordingSummary> {
        let _ = self.events.send(Event::Stop);
        match self.thread.take().map(JoinHandle::join) {
            Some(Ok(result)) => result,
            _ => bail!("the recording thread panicked"),
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.events.send(Event::Stop);
            let _ = thread.join();
        }
    }
}

/// The smallest recording, physical pixels a side. Media Foundation's
/// software H.264 encoder refuses 32×32 and takes 34×34; smaller than this
/// is no use as a video anyway.
pub const MIN_SIDE: u32 = 64;

/// Hardware encoders have minimum sizes of their own (NVENC's H.264 needs
/// at least 145×49, and fails only once frames arrive), so smaller
/// recordings use the software encoder, which is quick at such sizes.
const HARDWARE_MIN_SIDE: u32 = 256;

/// What of a `width`×`height` monitor is recorded for `region`: all of it
/// for `None`; otherwise the region on the monitor, grown about its centre
/// to [`MIN_SIDE`] if smaller, with even sizes, as NV12 requires.
pub fn recordable(region: Option<PhysicalRect>, width: u32, height: u32) -> PhysicalRect {
    let full = PhysicalRect::new(0, 0, width, height);
    let r = region.unwrap_or(full).clamp_to(width, height);
    let grow = |at: i32, side: u32, limit: u32| {
        if side >= MIN_SIDE || limit < MIN_SIDE {
            return (at, side.min(limit));
        }
        let start = at - (MIN_SIDE - side) as i32 / 2;
        (start.clamp(0, (limit - MIN_SIDE) as i32), MIN_SIDE)
    };
    let (x, w) = grow(r.x, r.width, width);
    let (y, h) = grow(r.y, r.height, height);
    PhysicalRect::new(x, y, w & !1, h & !1)
}

/// Whether `monitor` is still attached. Unplugging a display does not close
/// its capture: Windows sends black frames, then none, and a display
/// plugged back in is a new one. Its handle stops being valid the moment it
/// goes, before the black frames, so the app checks it when Windows says
/// the displays changed and tells the recorder with
/// [`Recorder::display_gone`].
pub fn attached(monitor: MonitorId) -> bool {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(hmonitor(monitor), &mut info) }.as_bool()
}

/// 100-nanosecond units, Media Foundation's and Windows.Graphics.Capture's.
const TICKS_PER_SECOND: i64 = 10_000_000;

/// Run the HDR analysis on every this-many frames (15 times a second at 60
/// fps); tone mapping uses the latest result. The exposure is smoothed over
/// half a second anyway, and the analysis was most of the recorder's GPU
/// work per frame (issue #15).
const ANALYSE_EVERY: u32 = 4;

/// How long a still screen goes without a repeated frame.
const HEARTBEAT: i64 = TICKS_PER_SECOND;

/// Now, in the clock Windows.Graphics.Capture stamps frames with:
/// QueryPerformanceCounter in 100-nanosecond units.
fn qpc_ticks() -> i64 {
    let (mut counter, mut frequency) = (0i64, 0i64);
    unsafe {
        let _ = QueryPerformanceCounter(&mut counter);
        let _ = QueryPerformanceFrequency(&mut frequency);
    }
    (counter as i128 * TICKS_PER_SECOND as i128 / frequency.max(1) as i128) as i64
}

fn record(
    options: RecordOptions,
    frames: Sender<Event>,
    inbox: Receiver<Event>,
    ready: Sender<Result<()>>,
) -> Result<RecordingSummary> {
    let setup = (|| {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).context("MFStartup failed")?;
        }
        Session::new(&options, frames)
    })();
    let mut session = match setup {
        Ok(session) => {
            let _ = ready.send(Ok(()));
            session
        }
        Err(e) => {
            let _ = ready.send(Err(anyhow::anyhow!("{e:#}")));
            return Err(e);
        }
    };
    let result = session.run(&inbox);
    drop(session);
    unsafe {
        let _ = MFShutdown();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display;

    #[test]
    fn attached_displays_are_present_and_a_stale_handle_is_not() {
        let monitors = display::enumerate().unwrap();
        assert!(!monitors.is_empty());
        for m in &monitors {
            let id = MonitorId(m.hmonitor.0 as u64);
            assert!(attached(id), "{} is attached", m.device_name);
        }
        // A handle that names no display, as one unplugged does. GitHub's
        // hosted runners have only a virtual display, and Windows there
        // reports this handle as attached too; there is nothing to unplug.
        if std::env::var_os("CI").is_none() {
            assert!(!attached(MonitorId(0x7FFF_0001)));
        }
    }

    #[test]
    fn interruptions_are_told_apart_by_their_error_codes() {
        use windows::core::{Error, HRESULT};
        let windows_error = |code: u32| {
            anyhow::Error::from(Error::from_hresult(HRESULT(code as i32)))
                .context("WriteSample failed")
        };
        assert_eq!(
            Interruption::from_error(&windows_error(0x887A_0005)),
            Interruption::DeviceLost
        );
        assert_eq!(
            Interruption::from_error(&windows_error(0x8007_0070)),
            Interruption::DiskFull
        );
        // Codes only in the text count too.
        let text = anyhow::anyhow!("copy failed: 0x887A0007");
        assert_eq!(Interruption::from_error(&text), Interruption::DeviceLost);
        let other = anyhow::anyhow!("the monitor changed size during the recording");
        assert!(matches!(
            Interruption::from_error(&other),
            Interruption::Failed(m) if m.contains("changed size")
        ));
        assert_eq!(
            Interruption::DisplayGone.describe(),
            "the display was disconnected or turned off"
        );
    }

    #[test]
    fn recorded_regions_are_even_on_screen_and_not_too_small() {
        let rect = |x, y, w, h| Some(PhysicalRect::new(x, y, w, h));
        // All of it, or the region with odd sizes made even.
        assert_eq!(
            recordable(None, 3840, 2160),
            PhysicalRect::new(0, 0, 3840, 2160)
        );
        assert_eq!(
            recordable(rect(10, 20, 1001, 777), 3840, 2160),
            PhysicalRect::new(10, 20, 1000, 776)
        );
        // Off the edge: cut to the monitor (40×60 here), then grown back
        // to the minimum inside it.
        assert_eq!(
            recordable(rect(3800, 2100, 400, 400), 3840, 2160),
            PhysicalRect::new(3776, 2096, 64, 64)
        );
        // Tiny: grown about its centre, kept on the monitor.
        assert_eq!(
            recordable(rect(100, 300, 20, 200), 3840, 2160),
            PhysicalRect::new(78, 300, 64, 200)
        );
        assert_eq!(
            recordable(rect(0, 0, 10, 10), 3840, 2160),
            PhysicalRect::new(0, 0, 64, 64)
        );
    }
}
