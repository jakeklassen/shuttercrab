//! Watching a live recording (issue #66, spike S0): its coded frames
//! decoded as they arrive, with no media player's buffering in the way.
//!
//! Pictures go through Windows' H.264 decoder on the graphics card and are
//! converted to BGRA as they come out. Sound goes through its AAC decoder
//! to the speakers, with a few hundredths of a second queued. Where the
//! sound has got to playing is the clock: a picture is shown once the
//! clock is within the lead of its time. The picture may run a little
//! ahead of the sound rather than wait for all of it to arrive; sound a
//! little after the picture goes unnoticed, sound before it does not.

mod sound;
mod video;

use super::{gpu::Gpu, play::Started};
use crate::live::{LiveFormat, LiveFrame, Track};
use anyhow::{Context, Result};
use std::{
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
use windows::Win32::{
    Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D},
    Media::MediaFoundation::*,
};

/// How far ahead of the sound a picture is shown at first, 100 ns ticks.
/// People notice sound coming after the picture from about 125 ms
/// (ITU-R BT.1359).
const LEAD: i64 = 600_000;

/// A live recording playing as its frames arrive.
pub struct LivePlayer {
    video: Mutex<video::Decoder>,
    pictures: Mutex<video::Pictures>,
    sound: Option<(Mutex<sound::Decoder>, sound::Output)>,
    /// How far ahead of the sound pictures are shown, 100 ns ticks.
    lead: AtomicI64,
    /// The newest picture's time, and the one drawn last.
    newest: AtomicI64,
    drawn: AtomicI64,
    gpu: Gpu,
    /// Last, so it ends after everything above is released.
    _started: Started,
}

impl LivePlayer {
    /// Play a recording of `format`. `failed` hears why the sound stopped,
    /// if it does; the pictures then show as they arrive.
    pub fn new(format: LiveFormat, failed: impl Fn(String) + Send + 'static) -> Result<Self> {
        let started = Started::new()?;
        let gpu = Gpu::video(None)?;
        let (w, h, fps) = (format.width, format.height, format.fps);
        let video = video::Decoder::new(&gpu, w, h, fps).context("cannot decode the pictures")?;
        let pictures =
            video::Pictures::new(&gpu, w, h, fps).context("cannot convert the pictures")?;
        let sound = match format.sound {
            Some(codec) => Some((
                Mutex::new(sound::Decoder::new(codec).context("cannot decode the sound")?),
                sound::Output::start(failed)?,
            )),
            None => None,
        };
        Ok(Self {
            video: Mutex::new(video),
            pictures: Mutex::new(pictures),
            sound,
            lead: AtomicI64::new(LEAD),
            newest: AtomicI64::new(i64::MIN),
            drawn: AtomicI64::new(i64::MIN),
            gpu,
            _started: started,
        })
    }

    /// The graphics card's device the pictures are on: [`LivePlayer::draw`]
    /// draws into a texture of it.
    pub fn device(&self) -> ID3D11Device {
        self.gpu.device.clone()
    }

    /// Decode the next frame as it arrives.
    pub fn push(&self, frame: LiveFrame) -> Result<()> {
        match frame.track {
            Track::Video => {
                self.newest.fetch_max(frame.time, Ordering::Relaxed);
                lock(&self.video).decode(&frame, |time, texture, index| {
                    lock(&self.pictures).add(&self.gpu, time, texture, index)
                })
            }
            Track::Sound => match &self.sound {
                Some((decoder, output)) => {
                    lock(decoder).decode(&frame, |time, samples| output.queue(time, samples))
                }
                None => Ok(()),
            },
        }
    }

    /// Draw the newest picture that is due into `target` (BGRA, the
    /// video's size), unless it is the one drawn last: whether it drew.
    pub fn draw(&self, target: &ID3D11Texture2D) -> Result<bool> {
        let due = self
            .clock()
            .map(|now| now + self.lead.load(Ordering::Relaxed));
        let drawn = lock(&self.pictures).draw(&self.gpu, target, due)?;
        if let Some(time) = drawn {
            self.drawn.store(time, Ordering::Relaxed);
        }
        Ok(drawn.is_some())
    }

    /// Where the sound has got to playing; `None` before it starts, or if
    /// it cannot play.
    fn clock(&self) -> Option<i64> {
        self.sound.as_ref().and_then(|(_, output)| output.clock())
    }

    /// How far the picture drawn last is behind the newest one received.
    pub fn behind(&self) -> Duration {
        ticks(self.newest.load(Ordering::Relaxed) - self.drawn.load(Ordering::Relaxed))
    }

    /// How far the sound playing is behind the newest picture received.
    pub fn sound_behind(&self) -> Option<Duration> {
        self.clock()
            .map(|now| ticks(self.newest.load(Ordering::Relaxed) - now))
    }

    /// How far ahead of the sound pictures are shown.
    pub fn lead(&self) -> Duration {
        ticks(self.lead.load(Ordering::Relaxed))
    }

    pub fn set_lead(&self, lead: Duration) {
        self.lead
            .store((lead.as_nanos() / 100) as i64, Ordering::Relaxed);
    }

    pub fn set_muted(&self, muted: bool) {
        if let Some((_, output)) = &self.sound {
            output.set_muted(muted);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// 100 ns ticks as a `Duration`, none if negative.
fn ticks(ticks: i64) -> Duration {
    Duration::from_nanos(ticks.max(0) as u64 * 100)
}

/// A frame as a Media Foundation sample, for a decoder.
fn media_sample(frame: &LiveFrame) -> Result<IMFSample> {
    unsafe {
        let len = frame.data.len() as u32;
        let buffer = MFCreateMemoryBuffer(len)?;
        let mut data = std::ptr::null_mut();
        buffer.Lock(&mut data, None, None)?;
        // SAFETY: Lock gives a buffer of at least `len` bytes until Unlock.
        std::ptr::copy_nonoverlapping(frame.data.as_ptr(), data, frame.data.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(len)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(frame.time)?;
        sample.SetSampleDuration(frame.duration)?;
        if frame.key {
            sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
        }
        Ok(sample)
    }
}

/// Run a decoder's output until it wants more input: each sample it gives
/// to `out`. `make` gives the sample to fill, for a decoder that does not
/// bring its own.
fn drain(
    transform: &IMFTransform,
    make: impl Fn() -> Result<Option<IMFSample>>,
    mut changed: impl FnMut() -> Result<()>,
    mut out: impl FnMut(IMFSample) -> Result<()>,
) -> Result<()> {
    loop {
        let mut output = [MFT_OUTPUT_DATA_BUFFER {
            pSample: std::mem::ManuallyDrop::new(make()?),
            ..Default::default()
        }];
        let mut status = 0;
        let result = unsafe { transform.ProcessOutput(0, &mut output, &mut status) };
        let [output] = output;
        let sample = std::mem::ManuallyDrop::into_inner(output.pSample);
        drop(std::mem::ManuallyDrop::into_inner(output.pEvents));
        match result {
            Ok(()) => {
                if let Some(sample) = sample {
                    out(sample)?;
                }
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => changed()?,
            Err(e) => return Err(anyhow::Error::from(e).context("a decoder failed")),
        }
    }
}

/// Hand a decoder `frame`, emptying its output first if it is full.
fn feed(
    transform: &IMFTransform,
    frame: &LiveFrame,
    mut empty: impl FnMut() -> Result<()>,
) -> Result<()> {
    let sample = media_sample(frame)?;
    loop {
        match unsafe { transform.ProcessInput(0, &sample, 0) } {
            Ok(()) => return Ok(()),
            Err(e) if e.code() == MF_E_NOTACCEPTING => empty()?,
            Err(e) => return Err(anyhow::Error::from(e).context("a decoder refused a frame")),
        }
    }
}

/// A decoder's output type: the first it offers that `wanted` accepts.
fn choose_output(transform: &IMFTransform, wanted: impl Fn(&IMFMediaType) -> bool) -> Result<()> {
    for index in 0.. {
        let offered = unsafe { transform.GetOutputAvailableType(0, index) }
            .map_err(|e| anyhow::Error::from(e).context("no output type the player can use"))?;
        if wanted(&offered) {
            unsafe { transform.SetOutputType(0, &offered, 0) }?;
            return Ok(());
        }
    }
    unreachable!()
}
