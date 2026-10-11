//! Screen recording on Linux under X11 (PRD §13): the area is read from the
//! root window at the frame rate ([`grab`]), with the pointer drawn in, and
//! encoded to H.264 in an MP4 by GStreamer ([`encoder`]). No sound yet.
//!
//! X11 says nothing when the screen changes, so a frame is read every
//! frame period. Paused time is left out of the timeline; a frame that
//! cannot be read and encoded in time makes the next ones be skipped, not
//! queued.

mod encoder;
mod grab;

pub use encoder::missing_software;

use super::service::X11;
use crate::{
    record::{Interruption, Microphone, RecordOptions, RecordingSummary, Source},
    screen::MonitorId,
};
use anyhow::{Context, Result, bail};
use encoder::Encoder;
use grab::Grab;
use std::{
    sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel},
    thread::JoinHandle,
    time::{Duration, Instant},
};

enum Event {
    Pause,
    Resume,
    Stop,
    DisplayGone,
    WindowClosed,
    ShowCursor(bool),
}

/// A recording in progress, on its own thread.
pub struct Recorder {
    events: Sender<Event>,
    thread: Option<JoinHandle<Result<RecordingSummary>>>,
    /// Resolves when the thread ends, however it ends.
    ended: Option<futures::channel::oneshot::Receiver<()>>,
}

impl Recorder {
    /// Start recording. Returns once the first frame can be read and the
    /// encoder is running.
    pub fn start(options: RecordOptions) -> Result<Self> {
        let (events, inbox) = channel();
        let (ready, started) = channel::<Result<()>>();
        // Dropped when the thread ends, which resolves `ended`.
        let (ending, ended) = futures::channel::oneshot::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("shuttercrab-record".into())
            .spawn(move || {
                let _ending = ending;
                record(&options, &inbox, &ready)
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

    /// No sound on Linux yet.
    pub fn set_sound(&self, _source: Source, _on: bool) {}

    pub fn set_sound_process(&self, _process: Option<u32>) {}

    /// Leave the pointer out for now (`false`), or put it back as the
    /// options say.
    pub fn show_cursor(&self, show: bool) {
        let _ = self.events.send(Event::ShowCursor(show));
    }

    /// The recorded window was closed: end the recording, keeping what was
    /// recorded.
    pub fn window_closed(&self) {
        let _ = self.events.send(Event::WindowClosed);
    }

    /// The recorded display is gone: end the recording, keeping what was
    /// recorded.
    pub fn display_gone(&self) {
        let _ = self.events.send(Event::DisplayGone);
    }

    /// Nothing to adjust: X11 has no HDR to follow.
    pub fn display_changed(&self) {}

    /// A future that resolves when the recording ends, by [`Recorder::stop`]
    /// or by itself. `None` after the first call.
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

/// Whether `monitor` is still attached.
pub fn attached(monitor: MonitorId) -> bool {
    X11::connect()
        .and_then(|x| x.monitors())
        .is_ok_and(|monitors| monitors.iter().any(|m| m.id == monitor))
}

/// No microphones on Linux yet.
pub fn microphones() -> Result<Vec<Microphone>> {
    Ok(Vec::new())
}

/// What `e`, an error during recording, means for the user.
pub(crate) fn interruption(e: &anyhow::Error) -> Interruption {
    let text = format!("{e:#}");
    if text.contains("No space left on device") {
        Interruption::DiskFull
    } else {
        Interruption::Failed(text)
    }
}

/// Record until told to stop, then finish the file. Says on `ready` once
/// recording has started, or why it could not.
fn record(
    options: &RecordOptions,
    inbox: &Receiver<Event>,
    ready: &Sender<Result<()>>,
) -> Result<RecordingSummary> {
    let started = (|| {
        let grab = Grab::new(options)?;
        // The first frame, read before saying recording started: one that
        // cannot be read fails now, with a reason.
        let first = grab.frame(options.include_cursor)?;
        let encoder = Encoder::start(&options.path, grab.size(), options.fps)?;
        Ok((grab, encoder, first))
    })();
    let (grab, encoder, first) = match started {
        Ok(started) => started,
        Err(e) => {
            let reason = format!("{e:#}");
            let _ = ready.send(Err(e));
            bail!("{reason}");
        }
    };
    let _ = ready.send(Ok(()));
    let (width, height) = grab.size();
    log::info!(
        "recording {width}×{height} at {} fps under X11",
        options.fps
    );

    let mut timeline = Timeline::new(options.fps);
    let mut pointer = options.include_cursor;
    let mut interrupted = None;
    let mut next = Some(first);
    loop {
        let event = if timeline.paused() {
            inbox.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            inbox.recv_timeout(timeline.until_next())
        };
        match event {
            Ok(Event::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Event::Pause) => timeline.pause(),
            Ok(Event::Resume) => timeline.resume(),
            Ok(Event::ShowCursor(show)) => pointer = show && options.include_cursor,
            Ok(Event::DisplayGone) => {
                interrupted = Some(Interruption::DisplayGone);
                break;
            }
            Ok(Event::WindowClosed) => {
                interrupted = Some(Interruption::WindowClosed);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                let frame = match next.take() {
                    Some(frame) => Ok(frame),
                    None => grab.frame(pointer),
                };
                let written =
                    frame.and_then(|frame| encoder.push(frame, timeline.now(), timeline.period));
                if let Err(e) = written {
                    log::error!("recording stopped: {e:#}");
                    interrupted = Some(interruption(&e));
                    break;
                }
                timeline.written();
            }
        }
    }
    let finished = encoder.finish();
    if let Err(e) = &finished {
        log::error!("could not finish the recording: {e:#}");
        if interrupted.is_none() {
            finished?;
        }
    }
    Ok(RecordingSummary {
        path: options.path.clone(),
        width,
        height,
        frames: timeline.frames,
        dropped_busy: 0,
        skipped_rate: timeline.skipped,
        duration: timeline.now(),
        paused: timeline.paused_for(),
        hardware_encoder: false,
        sound: false,
        interrupted,
        timing: None,
    })
}

/// The recording's clock: frame times at the frame rate, with paused time
/// left out.
struct Timeline {
    period: Duration,
    started: Instant,
    /// When the next frame is due.
    next: Instant,
    paused_since: Option<Instant>,
    paused: Duration,
    frames: u64,
    skipped: u64,
}

impl Timeline {
    fn new(fps: u32) -> Self {
        let now = Instant::now();
        Self {
            period: Duration::from_secs(1) / fps.max(1),
            started: now,
            next: now,
            paused_since: None,
            paused: Duration::ZERO,
            frames: 0,
            skipped: 0,
        }
    }

    fn paused(&self) -> bool {
        self.paused_since.is_some()
    }

    fn pause(&mut self) {
        self.paused_since.get_or_insert_with(Instant::now);
    }

    /// Carry on from now, with the paused time left out.
    fn resume(&mut self) {
        if let Some(since) = self.paused_since.take() {
            self.paused += since.elapsed();
            self.next = Instant::now();
        }
    }

    fn paused_for(&self) -> Duration {
        self.paused
            + self
                .paused_since
                .map_or(Duration::ZERO, |since| since.elapsed())
    }

    /// Time on the recording's timeline.
    fn now(&self) -> Duration {
        self.started.elapsed().saturating_sub(self.paused_for())
    }

    fn until_next(&self) -> Duration {
        self.next.saturating_duration_since(Instant::now())
    }

    /// A frame was written: the next is due a period later, or, if that
    /// time has passed, at the next period still ahead, skipping those
    /// missed.
    fn written(&mut self) {
        self.frames += 1;
        self.next += self.period;
        let now = Instant::now();
        if self.next < now {
            let behind = (now - self.next).as_nanos() / self.period.as_nanos();
            let behind = behind as u32 + 1;
            self.skipped += u64::from(behind);
            self.next += self.period * behind;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paused_time_is_left_out() {
        let mut timeline = Timeline::new(30);
        timeline.pause();
        std::thread::sleep(Duration::from_millis(50));
        timeline.resume();
        assert!(timeline.paused_for() >= Duration::from_millis(50));
        assert!(timeline.now() < Duration::from_millis(40));
    }

    #[test]
    fn a_late_frame_skips_the_missed_ones() {
        let mut timeline = Timeline::new(100);
        timeline.next = Instant::now() - Duration::from_millis(35);
        timeline.written();
        assert_eq!(timeline.frames, 1);
        assert_eq!(timeline.skipped, 3);
        assert!(timeline.until_next() <= Duration::from_millis(10));
    }
}
