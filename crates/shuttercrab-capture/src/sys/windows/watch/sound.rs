//! A live recording's sound: Windows' AAC decoder, then the speakers
//! through WASAPI, on a thread of its own. Enough is kept in hand for
//! frames that arrive unevenly: a hundredth of a second on one PC, more
//! over a network that delivers in bursts, learnt as it plays. When more
//! than that builds up, the queue plays a little faster until it is back,
//! so the sound never falls further behind. Where it has got to playing is
//! the player's clock.

use super::{choose_output, drain, feed, lock};
use crate::live::{LiveFrame, SoundCodec};
use crate::sys::imp::clock::{self, TICKS_PER_SECOND};
use anyhow::{Context, Result};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
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
        Threading::{CreateEventW, WaitForSingleObject},
    },
};

/// The sound's rate, frames a second, and channels: as the recorder
/// encodes it.
const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;

/// How much more than the spare is queued before it plays, frames: so the
/// first is a little over one AAC frame (1024), as they arrive one at a
/// time.
const FILL: usize = 620;

/// The least the queue should hold whenever the speakers ask for more, to
/// begin with and at best, frames (10 ms): enough on one PC.
const SPARE: usize = 480;

/// The most it is let grow to as sound arrives more unevenly, frames
/// (150 ms).
const MOST_SPARE: usize = 7_200;

/// How long the queue must not run dry before its spare shrinks.
const SETTLED: Duration = Duration::from_secs(10);

/// How much more than twice the spare may build up at once (after a
/// stall) before it is cut, frames (30 ms; 50 ms in all at the least).
const BACKLOG: usize = 1_440;

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

/// How long speakers must have played for, when they stop, to be opened
/// again.
const REOPEN_AFTER: Duration = Duration::from_secs(1);

fn frames_to_ticks(frames: usize) -> i64 {
    frames as i64 * TICKS_PER_SECOND / i64::from(RATE)
}

/// The sound as it arrives, to samples: AAC through Windows' decoder,
/// Opus through libopus, PCM as it is.
pub(super) enum Decoder {
    Aac(Aac),
    Opus(super::super::opus::Decoder),
    Pcm,
}

impl Decoder {
    pub(super) fn new(codec: SoundCodec) -> Result<Self> {
        Ok(match codec {
            SoundCodec::Aac => Self::Aac(Aac::new()?),
            SoundCodec::Opus => Self::Opus(super::super::opus::Decoder::new()?),
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
            Self::Opus(opus) => out(frame.time, opus.decode(&frame.data)?),
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
    /// Waiting for the spare and [`FILL`] frames before playing: at the
    /// start, and once the queue has run dry.
    filling: bool,
    /// The least the queue should hold, frames: [`SPARE`] to begin with,
    /// twice as much each time it runs dry, and an eighth less after each
    /// [`SETTLED`] that it does not. Zero until the first sound.
    spare: usize,
    /// When the spare last changed.
    spare_since: Option<Instant>,
    /// What the spare was before it last grew, until it settles: running
    /// dry because the sender paused (to start again at another quality)
    /// says nothing about the network, and is undone.
    grew_from: Option<usize>,
    /// Where the sound pushed last ends: the next should start there.
    next: Option<i64>,
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
    /// Start again from nothing, for speakers opened again: what was
    /// queued has missed its time.
    fn restart(&mut self) {
        self.frames.clear();
        self.filling = true;
        self.clock = None;
        self.next = None;
    }

    fn push(&mut self, time: i64, samples: &[i16]) {
        // Sound that does not follow on from what is queued (the sender
        // started its recorder again, after a pause) starts the queue
        // afresh at its own time, so the clock goes with it.
        if let Some(next) = self.next
            && (time - next).abs() > JUMP
        {
            log::info!(
                "live sound jumped {} ms {:?} in",
                (time - next) / 10_000,
                self.started.map(|s| s.elapsed())
            );
            self.frames.clear();
            self.filling = true;
            if let Some(spare) = self.grew_from.take() {
                self.spare = spare;
            }
        }
        if self.frames.is_empty() {
            self.front = time;
            self.position = 0.0;
        }
        self.next = Some(time + frames_to_ticks(samples.len() / 2));
        self.started.get_or_insert_with(Instant::now);
        self.spare = self.spare.max(SPARE);
        self.frames.extend(samples.as_chunks::<2>().0);
    }

    /// Keep more in hand after running dry, and a little less after a
    /// while without.
    fn adapt(&mut self, ran_dry: bool) {
        let settled = self
            .spare_since
            .is_none_or(|since| since.elapsed() >= SETTLED);
        let spare = if ran_dry {
            self.grew_from = Some(self.spare);
            (self.spare * 2).min(MOST_SPARE)
        } else if settled {
            self.grew_from = None;
            (self.spare - self.spare / 8).max(SPARE)
        } else {
            return;
        };
        if spare != self.spare {
            log::info!(
                "live sound keeps {} ms in hand {:?} in",
                frames_to_ticks(spare) / 10_000,
                self.started.map(|s| s.elapsed())
            );
        }
        self.spare = spare;
        self.spare_since = Some(Instant::now());
    }

    /// Fill `out` with the next sound, or silence while filling: the time
    /// of the frame after it, if it was all sound.
    fn fill(&mut self, out: &mut [i16]) -> Option<i64> {
        if self.filling || self.clock.is_none() {
            if self.frames.len() < self.spare.max(SPARE) + FILL {
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
                self.adapt(true);
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
        Some(self.front + (self.position * TICKS_PER_SECOND as f64 / f64::from(RATE)) as i64)
    }

    /// Drop the frames played.
    fn consume(&mut self) {
        let played = (self.position as usize).min(self.frames.len());
        self.frames.drain(..played);
        self.front += frames_to_ticks(played);
        self.position -= played as f64;
    }

    /// Play a little faster when the queue has held more than its spare
    /// all of the last half second, and a little slower when it has run
    /// close to dry: the sound stays just behind what arrives, without
    /// cutting any out. A backlog of twice the spare and [`BACKLOG`] (after
    /// a stall) is cut at once, back to the spare and [`FILL`], and faded in.
    fn keep_pace(&mut self) {
        self.adapt(false);
        let (queued, spare) = (self.frames.len(), self.spare);
        if queued > spare * 2 + BACKLOG {
            let cut = queued - (spare + FILL);
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
            self.pace = if self.low > spare + spare / 2 {
                1.0 + PACE
            } else if self.low < spare / 4 {
                1.0 - PACE
            } else {
                1.0
            };
            self.low = usize::MAX;
            self.since = Some(Instant::now());
        }
    }
}

/// The loudest a boosted sound is let reach: 1 dB under full scale.
const CEILING: f32 = 0.89 * i16::MAX as f32;

/// How quickly the limiter gives back what it took, a fraction a frame:
/// over about 150 ms.
const RELEASE: f32 = 1.0 / (0.15 * RATE as f32);

/// The viewer's volume: the sound raised or lowered by a gain, its peaks
/// held under [`CEILING`] so a boost never clips. Where a peak would go
/// over, the gain drops at once and comes back over about 150 ms.
#[derive(Debug)]
struct Volume {
    /// What the limiter lets through of the gain, 0 to 1.
    let_through: f32,
}

impl Default for Volume {
    fn default() -> Self {
        Self { let_through: 1.0 }
    }
}

impl Volume {
    /// `frames` (stereo), at `gain`.
    fn apply(&mut self, frames: &mut [i16], gain: f32) {
        for frame in frames.as_chunks_mut::<2>().0 {
            let peak = f32::from(frame[0].unsigned_abs().max(frame[1].unsigned_abs())) * gain;
            let room = if peak > CEILING { CEILING / peak } else { 1.0 };
            self.let_through = if room < self.let_through {
                room
            } else {
                self.let_through + (room - self.let_through) * RELEASE
            };
            for sample in frame {
                *sample = (f32::from(*sample) * gain * self.let_through)
                    .round()
                    .clamp(-CEILING, CEILING) as i16;
            }
        }
    }
}

struct Shared {
    queue: Mutex<Queue>,
    /// The volume, as a percentage of the sound as sent.
    volume: AtomicU32,
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
            volume: AtomicU32::new(100),
            muted: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            alive: AtomicBool::new(true),
        });
        let playing = shared.clone();
        let thread = std::thread::Builder::new()
            .name("live sound".into())
            .spawn(move || {
                let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
                // Speakers that stop after playing a while (headphones
                // unplugged, another default chosen) are opened again: the
                // default is then another device. Ones that fail at once
                // are given up on.
                loop {
                    let opened = Instant::now();
                    let Err(e) = play(&playing) else { break };
                    playing.alive.store(false, Ordering::Relaxed);
                    lock(&playing.queue).restart();
                    if playing.stop.load(Ordering::Acquire) || opened.elapsed() < REOPEN_AFTER {
                        failed(format!("{e:#}"));
                        break;
                    }
                    log::warn!("the speakers stopped, opening the default ones again: {e:#}");
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
        // With no speakers playing, nothing would ever take it.
        if self.shared.alive.load(Ordering::Relaxed) {
            lock(&self.shared.queue).push(time, &samples);
        }
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

    /// Play the sound at percent of its level as sent.
    pub(super) fn set_volume(&self, percent: u32) {
        self.shared.volume.store(percent, Ordering::Relaxed);
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
    let ago = clock::duration(clock::now() - qpc as i64);
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
        let mut volume = Volume::default();
        let mut fills = VecDeque::<(u64, Option<i64>)>::new();
        let mut written = 0u64;
        client.Start()?;
        shared.alive.store(true, Ordering::Relaxed);
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
            let percent = shared.volume.load(Ordering::Relaxed);
            if shared.muted.load(Ordering::Relaxed) {
                out.fill(0);
            } else if percent != 100 {
                volume.apply(out, percent as f32 / 100.0);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A full-scale stereo sine, `frames` long.
    fn sine(frames: usize, level: f32) -> Vec<i16> {
        (0..frames)
            .flat_map(|i| {
                let s = (level * i16::MAX as f32 * (i as f32 * 0.05).sin()) as i16;
                [s, s]
            })
            .collect()
    }

    #[test]
    fn a_boost_never_clips() {
        let mut volume = Volume::default();
        let mut loud = sine(RATE as usize, 1.0);
        volume.apply(&mut loud, 4.0);
        assert!(loud.iter().all(|&s| f32::from(s).abs() <= CEILING));
        // And it still plays near the top, not squashed quiet.
        assert!(loud.iter().map(|s| s.unsigned_abs()).max().unwrap() as f32 > CEILING * 0.95);
    }

    #[test]
    fn quiet_sound_is_raised_by_the_gain() {
        let mut volume = Volume::default();
        let quiet = sine(RATE as usize, 0.1);
        let mut raised = quiet.clone();
        volume.apply(&mut raised, 3.0);
        for (before, after) in quiet.iter().zip(&raised) {
            assert!((f32::from(*after) - f32::from(*before) * 3.0).abs() <= 1.0);
        }
    }

    #[test]
    fn the_limiter_lets_go_after_a_loud_moment() {
        let mut volume = Volume::default();
        let mut loud = sine(RATE as usize / 10, 1.0);
        volume.apply(&mut loud, 4.0);
        assert!(volume.let_through < 0.5);
        // A second of quiet: the full gain comes back.
        let mut quiet = sine(RATE as usize, 0.05);
        volume.apply(&mut quiet, 4.0);
        assert!(volume.let_through > 0.99, "{}", volume.let_through);
    }
    fn playing() -> Queue {
        let mut queue = Queue {
            clock: Some(Clock {
                at: Instant::now(),
                time: 0,
                running: true,
            }),
            // As a new player's queue: it starts by filling.
            filling: true,
            ..Queue::default()
        };
        queue.push(0, &vec![0; (SPARE + FILL) * 2]);
        queue
    }

    #[test]
    fn running_dry_keeps_more_in_hand_from_then_on() {
        let mut queue = playing();
        let mut out = vec![0; 480 * 2];
        assert!(queue.fill(&mut out).is_some());
        // Nothing more arrives: it runs dry.
        while queue.fill(&mut out).is_some() {}
        assert_eq!(queue.spare, SPARE * 2);
        // It waits for the larger spare before playing again.
        queue.push(0, &vec![0; (SPARE + FILL) * 2]);
        assert!(queue.fill(&mut out).is_none());
        queue.push(0, &vec![0; SPARE * 2]);
        assert!(queue.fill(&mut out).is_some());
    }

    #[test]
    fn sound_resuming_later_starts_the_queue_afresh_at_its_time() {
        let mut queue = playing();
        let mut out = vec![0; 480 * 2];
        // The sender pauses: the queue runs dry, and keeps more in hand.
        while queue.fill(&mut out).is_some() {}
        assert_eq!(queue.spare, SPARE * 2);
        // Its sound comes back half a second on, after what was queued is
        // gone.
        queue.frames.clear();
        let later = queue.next.unwrap() + TICKS_PER_SECOND / 2;
        queue.push(later, &[0; 4 * 2]);
        assert_eq!(queue.front, later);
        assert_eq!(queue.frames.len(), 4);
        assert_eq!(queue.spare, SPARE, "a pause says nothing about the network");
    }

    #[test]
    fn the_spare_grows_no_further_than_its_most() {
        let mut queue = playing();
        for _ in 0..10 {
            queue.adapt(true);
        }
        assert_eq!(queue.spare, MOST_SPARE);
    }
}
