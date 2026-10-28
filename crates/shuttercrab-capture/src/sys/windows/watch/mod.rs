//! Watching a live recording (issue #66, spike S0): its coded frames
//! decoded as they arrive, with no media player's buffering in the way.
//!
//! Pictures go through Windows' H.264 decoder on the graphics card and are
//! converted to BGRA as they come out. Sound goes as it came (PCM), or
//! through Windows' AAC decoder, to the speakers with a few hundredths of a
//! second queued. Where the sound has got to playing is the clock: a
//! picture is shown when the clock reaches its time, so they stay together
//! however late both arrive.

mod sound;
mod video;

use super::{gpu::Gpu, play::Started};
use crate::live::{LiveFormat, LiveFrame, Track, VideoCodec};
use anyhow::{Context, Result};
use std::{
    collections::VecDeque,
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    time::Duration,
};
use windows::Win32::{
    Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D},
    Media::MediaFoundation::*,
};

/// How far ahead of the sound a picture is shown at first, 100 ns ticks:
/// none. Sound a little after the picture goes unnoticed (up to about
/// 125 ms, ITU-R BT.1359), and showing it ahead would save about 45 ms; but
/// a voice over the screen should match it.
const LEAD: i64 = 0;

/// A live recording playing as its frames arrive.
pub struct LivePlayer {
    /// Coded pictures waiting until they are due: decoding one takes a
    /// millisecond or two, and they are small, where decoded ones are not.
    coded: Mutex<VecDeque<LiveFrame>>,
    video: Mutex<video::Decoder>,
    pictures: Mutex<video::Pictures>,
    sound: Option<(Mutex<sound::Decoder>, sound::Output)>,
    /// How far ahead of the sound pictures are shown, 100 ns ticks.
    lead: AtomicI64,
    /// The newest picture's time, and the one drawn last.
    newest: AtomicI64,
    drawn: AtomicI64,
    /// A picture converted but not yet drawn: the target was another size.
    undrawn: AtomicBool,
    fps: u32,
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
        let video = video::Decoder::new(&gpu, w, h, fps, format.video)
            .context("cannot decode the pictures")?;
        let pictures =
            video::Pictures::new(&gpu, video.kind(), fps).context("cannot convert the pictures")?;
        let sound = match format.sound {
            Some(codec) => Some((
                Mutex::new(sound::Decoder::new(codec).context("cannot decode the sound")?),
                sound::Output::start(failed)?,
            )),
            None => None,
        };
        Ok(Self {
            coded: Mutex::default(),
            video: Mutex::new(video),
            pictures: Mutex::new(pictures),
            sound,
            lead: AtomicI64::new(LEAD),
            newest: AtomicI64::new(i64::MIN),
            drawn: AtomicI64::new(i64::MIN),
            undrawn: AtomicBool::new(false),
            fps,
            gpu,
            _started: started,
        })
    }

    /// Whether this PC can play pictures coded as `codec`: H.264 always;
    /// HEVC and AV1 with Windows' extensions for them installed.
    pub fn decodes(codec: VideoCodec) -> bool {
        video::decodes(codec)
    }

    /// The graphics card's device the pictures are on: [`LivePlayer::draw`]
    /// draws into a texture of it.
    pub fn device(&self) -> ID3D11Device {
        self.gpu.device.clone()
    }

    /// Take the next frame as it arrives: sound is decoded and queued to
    /// play, a picture kept until it is due.
    pub fn push(&self, frame: LiveFrame) -> Result<()> {
        match frame.track {
            Track::Video => {
                self.newest.fetch_max(frame.time, Ordering::Relaxed);
                lock(&self.coded).push_back(frame);
                Ok(())
            }
            Track::Sound => match &self.sound {
                Some((decoder, output)) => {
                    lock(decoder).decode(&frame, |time, samples| output.queue(time, samples))
                }
                None => Ok(()),
            },
        }
    }

    /// The pictures' size now: it changes where the stream does, and the
    /// target [`LivePlayer::draw`] draws into should follow.
    pub fn size(&self) -> (u32, u32) {
        lock(&self.pictures).kind().size
    }

    /// Decode the pictures that are due and draw the newest into `target`
    /// (BGRA, of [`LivePlayer::size`]), unless none has fallen due since
    /// the last draw: whether it drew. A picture of another size than
    /// `target` waits for a target of its size.
    pub fn draw(&self, target: &ID3D11Texture2D) -> Result<bool> {
        let due = self
            .clock()
            .map(|now| now + self.lead.load(Ordering::Relaxed));
        let frames: Vec<LiveFrame> = {
            let mut coded = lock(&self.coded);
            let count = match due {
                Some(due) => coded.iter().take_while(|f| f.time <= due).count(),
                None => coded.len(),
            };
            coded.drain(..count).collect()
        };
        let (mut decoder, mut pictures) = (lock(&self.video), lock(&self.pictures));
        let mut newest = None;
        for frame in &frames {
            decoder.decode(frame, |time, texture, index, kind| {
                if pictures.kind() != kind {
                    *pictures = video::Pictures::new(&self.gpu, kind, self.fps)
                        .context("cannot convert the pictures as they now are")?;
                }
                pictures.convert(&self.gpu, texture, index)?;
                newest = Some(time);
                Ok(())
            })?;
        }
        let time = match newest {
            Some(time) => time,
            None if self.undrawn.load(Ordering::Relaxed) => self.drawn.load(Ordering::Relaxed),
            None => return Ok(false),
        };
        self.drawn.store(time, Ordering::Relaxed);
        let copied = pictures.copy_to(&self.gpu, target);
        self.undrawn.store(!copied, Ordering::Relaxed);
        Ok(copied)
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
