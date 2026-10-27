//! A live recording's sound: Windows' AAC decoder, then the speakers
//! through WASAPI, on a thread of its own. A few hundredths of a second
//! are queued, for frames that arrive unevenly; when more builds up, the
//! oldest is skipped, so the sound never falls further behind. Where it
//! has got to playing is the player's clock.

use super::{choose_output, drain, feed, lock};
use crate::live::{LiveFrame, SoundCodec};
use anyhow::{Context, Result};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
    Media::{Audio::*, MediaFoundation::*},
    System::{
        Com::{
            CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance,
            CoInitializeEx, CoUninitialize,
        },
        Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
        Threading::{CreateEventW, WaitForSingleObject},
    },
};

/// The sound's rate, frames a second, and channels: as the recorder
/// encodes it.
const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;

/// 100 ns ticks a second.
const TICKS: i64 = 10_000_000;

/// How much sound is queued before it plays, frames: a little over one
/// AAC frame (1024), as they arrive one at a time.
const QUEUED: usize = 1_100;

/// The least the queue should hold whenever the speakers ask for more,
/// frames (10 ms).
const SPARE: usize = 480;

/// How much may build up at once (after a stall) before it is cut, frames
/// (50 ms).
const MOST: usize = 2_400;

/// How much faster or slower the queue plays to keep near [`SPARE`]: 0.5%,
/// a pitch change of 9 cents, too little to hear in speech.
const PACE: f64 = 0.005;

/// How long sound fades in after a start or a cut, frames (5 ms).
const FADE: usize = 240;

/// How much the speakers' device holds, 100 ns ticks.
const DEVICE_BUFFER: i64 = 200_000;

/// How long the thread waits for the device before checking whether to
/// stop.
const WAKE_MS: u32 = 100;

fn frames_to_ticks(frames: usize) -> i64 {
    frames as i64 * TICKS / i64::from(RATE)
}

/// The sound as it arrives, to samples: AAC through Windows' decoder, PCM
/// as it is.
pub(super) enum Decoder {
    Aac(Aac),
    Pcm,
}

impl Decoder {
    pub(super) fn new(codec: SoundCodec) -> Result<Self> {
        Ok(match codec {
            SoundCodec::Aac => Self::Aac(Aac::new()?),
            SoundCodec::Pcm => Self::Pcm,
        })
    }

    /// Decode `frame`: the sound that comes out, its time and 16-bit
    /// stereo samples, to `out`.
    pub(super) fn decode(
        &mut self,
        frame: &LiveFrame,
        mut out: impl FnMut(i64, Vec<i16>) -> Result<()>,
    ) -> Result<()> {
        match self {
            Self::Aac(aac) => aac.decode(frame, out),
            Self::Pcm => out(
                frame.time,
                frame
                    .data
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&bytes| i16::from_le_bytes(bytes))
                    .collect(),
            ),
        }
    }
}

pub(super) struct Aac {
    transform: IMFTransform,
    /// The size of the samples to give it for its output, if it does not
    /// bring its own.
    output_size: Option<u32>,
}

// SAFETY: Media Foundation's AAC decoder is free-threaded (created in the
// multithreaded apartment); the player uses it from one thread at a time,
// behind a mutex.
unsafe impl Send for Aac {}

impl Aac {
    fn new() -> Result<Self> {
        unsafe {
            let transform: IMFTransform =
                CoCreateInstance(&CLSID_MSAACDecMFT, None, CLSCTX_INPROC_SERVER)
                    .context("no AAC decoder")?;
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
            input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE)?;
            input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, u32::from(CHANNELS))?;
            input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            // ADTS: each frame carries its own header.
            input.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 1)?;
            input.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?;
            // The rest of HEAACWAVEINFO after WAVEFORMATEX: the payload type
            // and profile again, then zeros.
            input.SetBlob(&MF_MT_USER_DATA, &[1, 0, 0x29, 0, 0, 0, 0, 0, 0, 0, 0, 0])?;
            transform
                .SetInputType(0, &input, 0)
                .context("the AAC decoder does not take this sound")?;
            choose_output(&transform, |offered| {
                offered.GetGUID(&MF_MT_SUBTYPE).ok() == Some(MFAudioFormat_PCM)
                    && offered.GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE).ok() == Some(16)
            })?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let info = transform.GetOutputStreamInfo(0)?;
            let brings = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0
                | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
            Ok(Self {
                transform,
                output_size: (info.dwFlags & brings == 0).then_some(info.cbSize.max(16_384)),
            })
        }
    }

    fn decode(
        &mut self,
        frame: &LiveFrame,
        mut out: impl FnMut(i64, Vec<i16>) -> Result<()>,
    ) -> Result<()> {
        let (transform, size) = (&self.transform, self.output_size);
        let make = || -> Result<Option<IMFSample>> {
            let Some(size) = size else {
                return Ok(None);
            };
            unsafe {
                let sample = MFCreateSample()?;
                sample.AddBuffer(&MFCreateMemoryBuffer(size)?)?;
                Ok(Some(sample))
            }
        };
        let mut take = |sample: IMFSample| -> Result<()> {
            unsafe {
                let buffer = sample.ConvertToContiguousBuffer()?;
                let (mut data, mut length) = (std::ptr::null_mut(), 0);
                buffer.Lock(&mut data, None, Some(&mut length))?;
                // SAFETY: Lock gives `length` readable bytes at `data`
                // until Unlock; 16-bit samples, aligned as allocated.
                let samples =
                    std::slice::from_raw_parts(data.cast::<i16>(), length as usize / 2).to_vec();
                buffer.Unlock()?;
                out(sample.GetSampleTime().unwrap_or(frame.time), samples)
            }
        };
        let mut empty = || drain(transform, make, || Ok(()), &mut take);
        feed(transform, frame, &mut empty)?;
        empty()
    }
}

/// Where the sound had got to playing, and when.
#[derive(Clone, Copy)]
struct Clock {
    at: Instant,
    time: i64,
    /// Whether it is playing on; not while the queue has run dry.
    running: bool,
}

impl Clock {
    fn now(&self) -> i64 {
        self.at_instant(Instant::now())
    }

    fn at_instant(&self, now: Instant) -> i64 {
        if self.running {
            self.time + (now.saturating_duration_since(self.at).as_nanos() / 100) as i64
        } else {
            self.time
        }
    }

    /// Move towards `time` at `at`, where the speakers say they were: a
    /// small difference a tenth at a time, so the pictures it paces stay
    /// even (each reading wobbles by a few ms); a large one at once.
    fn steer(&mut self, time: i64, at: Instant) {
        let now = Instant::now();
        let said = time + (now.saturating_duration_since(at).as_nanos() / 100) as i64;
        let current = self.at_instant(now);
        let error = said - current;
        *self = Clock {
            at: now,
            time: if error.abs() > JUMP {
                said
            } else {
                current + error / 10
            },
            running: true,
        };
    }
}

/// A difference between the clock and the speakers that is put right at
/// once, 100 ns ticks (50 ms).
const JUMP: i64 = 500_000;

/// Decoded sound waiting to play, one stereo frame after another (the
/// sender's sound has no gaps).
#[derive(Default)]
struct Queue {
    frames: VecDeque<[i16; 2]>,
    /// The time of the first frame queued.
    front: i64,
    /// Where between the first frame and the next playing has got.
    position: f64,
    /// How fast the queue plays: 1, or a little over to drain extra sound,
    /// or a little under to build some up.
    pace: f64,
    /// Waiting for [`QUEUED`] frames before playing: at the start, and
    /// once the queue has run dry.
    filling: bool,
    /// Frames into fading in, after a start or a cut.
    fade: usize,
    clock: Option<Clock>,
    /// The fewest frames queued when the speakers asked for more, since
    /// `since`.
    low: usize,
    since: Option<Instant>,
    /// When the first sound arrived, for the log.
    started: Option<Instant>,
}

impl Queue {
    fn push(&mut self, time: i64, samples: &[i16]) {
        if self.frames.is_empty() {
            self.front = time;
            self.position = 0.0;
        }
        self.started.get_or_insert_with(Instant::now);
        self.frames.extend(samples.as_chunks::<2>().0);
    }

    /// Fill `out` with the next sound, or silence while filling: the time
    /// of the frame after it, if it was all sound.
    fn fill(&mut self, out: &mut [i16]) -> Option<i64> {
        if self.filling || self.clock.is_none() {
            if self.frames.len() < QUEUED {
                out.fill(0);
                return None;
            }
            self.filling = false;
            self.fade = 0;
            self.pace = 1.0;
            self.low = usize::MAX;
            self.since = Some(Instant::now());
        }
        self.keep_pace();
        for (i, frame) in out.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let index = self.position as usize;
            if index + 1 >= self.frames.len() {
                let silent = out.len() / 2 - i;
                out[i * 2..].fill(0);
                self.filling = true;
                log::info!(
                    "live sound ran dry {:?} in: {} ms of silence played",
                    self.started.map(|s| s.elapsed()),
                    frames_to_ticks(silent) / 10_000
                );
                self.consume();
                return None;
            }
            // Between two frames when the pace is not 1.
            let (a, b, t) = (
                self.frames[index],
                self.frames[index + 1],
                self.position.fract(),
            );
            let gain = (self.fade as f64 / FADE as f64).min(1.0);
            for c in 0..2 {
                let value = f64::from(a[c]) + (f64::from(b[c]) - f64::from(a[c])) * t;
                frame[c] = (value * gain) as i16;
            }
            self.fade += 1;
            self.position += self.pace;
        }
        self.consume();
        Some(self.front + (self.position * TICKS as f64 / f64::from(RATE)) as i64)
    }

    /// Drop the frames played.
    fn consume(&mut self) {
        let played = (self.position as usize).min(self.frames.len());
        self.frames.drain(..played);
        self.front += frames_to_ticks(played);
        self.position -= played as f64;
    }

    /// Play a little faster when the queue has held more than [`SPARE`]
    /// all of the last half second, and a little slower when it has run
    /// close to dry: the sound stays a few ms behind what arrives, without
    /// cutting any out. A backlog of [`MOST`] (after a stall) is cut at
    /// once, back to [`QUEUED`], and faded in.
    fn keep_pace(&mut self) {
        let queued = self.frames.len();
        if queued > MOST {
            let cut = queued - QUEUED;
            log::info!(
                "live sound skipped {:?} in: {} ms",
                self.started.map(|s| s.elapsed()),
                frames_to_ticks(cut) / 10_000
            );
            self.frames.drain(..cut);
            self.front += frames_to_ticks(cut);
            self.fade = 0;
        }
        self.low = self.low.min(self.frames.len());
        if self
            .since
            .is_some_and(|since| since.elapsed().as_millis() >= 500)
        {
            self.pace = if self.low > SPARE + SPARE / 2 {
                1.0 + PACE
            } else if self.low < SPARE / 4 {
                1.0 - PACE
            } else {
                1.0
            };
            self.low = usize::MAX;
            self.since = Some(Instant::now());
        }
    }
}

struct Shared {
    queue: Mutex<Queue>,
    muted: AtomicBool,
    stop: AtomicBool,
    /// Whether the speakers are playing; once they fail, there is no clock.
    alive: AtomicBool,
}

/// The speakers, playing the queue on a thread of their own.
pub(super) struct Output {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Output {
    pub(super) fn start(failed: impl Fn(String) + Send + 'static) -> Result<Self> {
        let shared = Arc::new(Shared {
            queue: Mutex::default(),
            muted: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            alive: AtomicBool::new(true),
        });
        let playing = shared.clone();
        let thread = std::thread::Builder::new()
            .name("live sound".into())
            .spawn(move || {
                let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
                if let Err(e) = play(&playing) {
                    playing.alive.store(false, Ordering::Relaxed);
                    failed(format!("{e:#}"));
                }
                if com {
                    unsafe { CoUninitialize() };
                }
            })?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    pub(super) fn queue(&self, time: i64, samples: Vec<i16>) -> Result<()> {
        lock(&self.shared.queue).push(time, &samples);
        Ok(())
    }

    /// Where the sound has got to playing, on the recording's timeline.
    pub(super) fn clock(&self) -> Option<i64> {
        if !self.shared.alive.load(Ordering::Relaxed) {
            return None;
        }
        lock(&self.shared.queue).clock.map(|clock| clock.now())
    }

    pub(super) fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The moment a performance counter reading (100 ns ticks, as the
/// speakers' clock gives it) stands for.
fn instant_at(qpc: u64) -> Instant {
    let (mut count, mut frequency) = (0, 1);
    unsafe {
        let _ = QueryPerformanceCounter(&mut count);
        let _ = QueryPerformanceFrequency(&mut frequency);
    }
    let now = (count as u128 * 10_000_000 / frequency.max(1) as u128) as u64;
    let ago = Duration::from_nanos(now.saturating_sub(qpc) * 100);
    Instant::now().checked_sub(ago).unwrap_or_else(Instant::now)
}

/// An event handle, closed when dropped.
struct Event(HANDLE);

impl Drop for Event {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Play the queue on the default speakers until told to stop.
fn play(shared: &Shared) -> Result<()> {
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .context("no audio device enumerator")?;
        let device = devices
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .context("no speakers")?;
        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .context("could not open the speakers")?;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: CHANNELS,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * u32::from(CHANNELS) * 2,
            nBlockAlign: CHANNELS * 2,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        // Windows converts to the device's own format.
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                DEVICE_BUFFER,
                0,
                &format,
                None,
            )
            .context("could not start playing sound")?;
        let event = Event(CreateEventW(None, false, false, None).context("no event")?);
        client.SetEventHandle(event.0)?;
        let render: IAudioRenderClient = client.GetService()?;
        let speakers: IAudioClock = client.GetService()?;
        let frequency = speakers.GetFrequency()?;
        let size = client.GetBufferSize()?;
        // Each fill: the frames written to the device by its end, and the
        // time just after its last, if it was sound.
        let mut fills = VecDeque::<(u64, Option<i64>)>::new();
        let mut written = 0u64;
        client.Start()?;
        while !shared.stop.load(Ordering::Acquire) {
            if WaitForSingleObject(event.0, WAKE_MS) != WAIT_OBJECT_0 {
                continue;
            }
            let free = size - client.GetCurrentPadding()?;
            if free == 0 {
                continue;
            }
            let data = render.GetBuffer(free)?;
            // SAFETY: GetBuffer gives room for `free` frames of 16-bit
            // stereo until ReleaseBuffer.
            let out = std::slice::from_raw_parts_mut(
                data.cast::<i16>(),
                free as usize * usize::from(CHANNELS),
            );
            let end = lock(&shared.queue).fill(out);
            if shared.muted.load(Ordering::Relaxed) {
                out.fill(0);
            }
            render.ReleaseBuffer(free, 0)?;
            written += u64::from(free);
            fills.push_back((written, end));
            if fills.len() > 64 {
                fills.pop_front();
            }

            // Which frame the speakers are playing, and when that was.
            let (mut position, mut qpc) = (0, 0);
            speakers.GetPosition(&mut position, Some(&mut qpc))?;
            let played = position * u64::from(RATE) / frequency;
            let playing = fills
                .iter()
                .find(|(end, _)| *end > played)
                .and_then(|&(end, time)| Some(time? - frames_to_ticks((end - played) as usize)));
            let at = instant_at(qpc);
            let mut queue = lock(&shared.queue);
            queue.clock = match (playing, queue.clock) {
                (Some(time), Some(mut clock)) if clock.running => {
                    clock.steer(time, at);
                    Some(clock)
                }
                (Some(time), _) => Some(Clock {
                    at,
                    time,
                    running: true,
                }),
                // Playing silence (run dry): the clock waits for the sound.
                (None, Some(clock)) => Some(Clock {
                    at: Instant::now(),
                    time: clock.now(),
                    running: false,
                }),
                (None, None) => None,
            };
        }
        client.Stop()?;
        Ok(())
    }
}
