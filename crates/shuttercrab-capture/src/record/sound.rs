//! A recording's sound: what the speakers play (Windows' loopback, WASAPI,
//! on the default output device), and a microphone, each captured on a
//! thread of its own while switched on.
//!
//! Windows converts each to the track's format (48 kHz, 16-bit stereo),
//! whatever the device runs at, and stamps each packet with the time of its
//! first sample on the same clock as the video frames (QPC). The recording
//! thread mixes them with a [`Mixer`]: each source's packets go where their
//! times say, a source's gaps stay silent, overlaps are trimmed, and the
//! track is written out a little behind now, so sound never drifts from the
//! picture. A microphone switched off is not opened at all, so Windows does
//! not show it as in use.

use super::{Event, TICKS_PER_SECOND};
use anyhow::{Context, Result};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread::JoinHandle,
};
use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        Media::Audio::{
            AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            DEVICE_STATE_ACTIVE, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator,
            MMDeviceEnumerator, WAVE_FORMAT_PCM, WAVEFORMATEX, eCapture, eConsole, eRender,
        },
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
                CoUninitialize, STGM_READ,
            },
            Threading::{CreateEventW, WaitForSingleObject},
        },
    },
    core::{HSTRING, PCWSTR},
};

/// The track's sample rate, frames a second.
pub(super) const RATE: u32 = 48_000;

/// The track's channels: stereo.
pub(super) const CHANNELS: u16 = 2;

/// Where a recording's sound comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// What the speakers play.
    System,
    /// A microphone.
    Microphone,
}

impl Source {
    fn index(self) -> usize {
        match self {
            Source::System => 0,
            Source::Microphone => 1,
        }
    }
}

/// A microphone Windows knows of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Microphone {
    /// Windows' id for it, to record from it.
    pub id: String,
    /// Its name, as Windows' sound settings show it.
    pub name: String,
}

/// The microphones plugged in and on, Windows' default first.
pub fn microphones() -> Result<Vec<Microphone>> {
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .context("no audio device enumerator")?;
        let default = devices
            .GetDefaultAudioEndpoint(eCapture, eConsole)
            .ok()
            .and_then(|d| device_id(&d).ok());
        let all = devices.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        let mut found = Vec::new();
        for i in 0..all.GetCount()? {
            let device = all.Item(i)?;
            found.push(Microphone {
                id: device_id(&device)?,
                name: device_name(&device).unwrap_or_else(|_| "Microphone".into()),
            });
        }
        found.sort_by_key(|m| Some(&m.id) != default.as_ref());
        Ok(found)
    }
}

fn device_id(device: &IMMDevice) -> Result<String> {
    unsafe {
        let id = device.GetId()?;
        let text = id.to_string();
        CoTaskMemFree(Some(id.0.cast()));
        Ok(text?)
    }
}

fn device_name(device: &IMMDevice) -> Result<String> {
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ)?;
        let name = store.GetValue(&PKEY_Device_FriendlyName)?;
        Ok(name.to_string())
    }
}

/// Sound from one source, captured from `captured` (QPC ticks, its first
/// frame) on: 16-bit samples, the channels interleaved.
pub(super) struct Packet {
    pub(super) source: Source,
    pub(super) captured: i64,
    pub(super) samples: Vec<i16>,
}

/// How much sound a device buffers, 100 ns ticks: room for the thread to be
/// late now and then.
const BUFFER: i64 = TICKS_PER_SECOND / 5;

/// How long a thread waits for sound before checking whether to stop.
const WAKE_MS: u32 = 100;

/// One source's capture, running on its own thread until dropped.
pub(super) struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Start capturing `source` (a microphone: `device`, or Windows'
    /// default), sending each packet to the recording thread as
    /// [`Event::Sound`]. Returns once the capture is running, or why it
    /// could not start.
    pub(super) fn start(
        source: Source,
        device: Option<String>,
        events: Sender<Event>,
    ) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready, started) = std::sync::mpsc::channel::<Result<()>>();
        let stopping = stop.clone();
        let name = match source {
            Source::System => "shuttercrab-system-sound",
            Source::Microphone => "shuttercrab-microphone",
        };
        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || capture(source, device.as_deref(), &events, &stopping, ready))
            .context("could not start the sound thread")?;
        match started.recv() {
            Ok(Ok(())) => Ok(Self {
                stop,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                anyhow::bail!("the sound thread stopped")
            }
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A capture thread: open the device, say whether it started, then pass on
/// packets until told to stop. A device that goes away (headphones
/// unplugged) is replaced by the new default one.
fn capture(
    source: Source,
    device: Option<&str>,
    events: &Sender<Event>,
    stop: &AtomicBool,
    ready: std::sync::mpsc::Sender<Result<()>>,
) {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let mut ready = Some(ready);
    // The chosen microphone first; once it has gone, the default one.
    let mut device = device;
    while !stop.load(Ordering::Acquire) {
        let stream = match Stream::open(source, device) {
            Ok(stream) => stream,
            Err(e) => {
                match ready.take() {
                    Some(ready) => {
                        let _ = ready.send(Err(e));
                        break;
                    }
                    // Mid-recording: wait for a device, and try again.
                    None => log::warn!("no {source:?} device to record: {e:#}"),
                }
                device = None;
                std::thread::sleep(std::time::Duration::from_millis(500));
                continue;
            }
        };
        if let Some(ready) = ready.take() {
            let _ = ready.send(Ok(()));
        }
        match stream.pass_on(source, events, stop) {
            Ok(()) => break,
            Err(e) => {
                log::warn!("the {source:?} device changed or went away: {e:#}; reopening");
                device = None;
            }
        }
    }
    if com {
        unsafe { CoUninitialize() };
    }
}

/// An open capture stream: the output device's loopback, or a microphone.
struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
}

impl Stream {
    fn open(source: Source, id: Option<&str>) -> Result<Self> {
        unsafe {
            let devices: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .context("no audio device enumerator")?;
            let device = match (source, id) {
                (Source::System, _) => devices
                    .GetDefaultAudioEndpoint(eRender, eConsole)
                    .context("no default output device")?,
                (Source::Microphone, Some(id)) => devices
                    .GetDevice(PCWSTR(HSTRING::from(id).as_ptr()))
                    .context("the chosen microphone is not there")?,
                (Source::Microphone, None) => devices
                    .GetDefaultAudioEndpoint(eCapture, eConsole)
                    .context("no microphone")?,
            };
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .context("could not open the sound device")?;
            let format = WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_PCM as u16,
                nChannels: CHANNELS,
                nSamplesPerSec: RATE,
                nAvgBytesPerSec: RATE * u32::from(CHANNELS) * 2,
                nBlockAlign: CHANNELS * 2,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            // Windows converts from the device's own format to the track's.
            let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            if source == Source::System {
                flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
            }
            client
                .Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER, 0, &format, None)
                .context("could not start capturing sound")?;
            let event = CreateEventW(None, false, false, None).context("no event")?;
            let opened = (|| {
                client.SetEventHandle(event)?;
                let capture: IAudioCaptureClient = client.GetService()?;
                client.Start()?;
                anyhow::Ok(capture)
            })();
            match opened {
                Ok(capture) => Ok(Self {
                    client,
                    capture,
                    event,
                }),
                Err(e) => {
                    let _ = CloseHandle(event);
                    Err(e.context("could not start capturing sound"))
                }
            }
        }
    }

    /// Send every packet on until `stop`; an error means the device went
    /// away.
    fn pass_on(&self, source: Source, events: &Sender<Event>, stop: &AtomicBool) -> Result<()> {
        while !stop.load(Ordering::Acquire) {
            let woken = unsafe { WaitForSingleObject(self.event, WAKE_MS) };
            if woken != WAIT_OBJECT_0 {
                continue;
            }
            while unsafe { self.capture.GetNextPacketSize() }? > 0 {
                let packet = self.next_packet(source)?;
                if events.send(Event::Sound(packet)).is_err() {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn next_packet(&self, source: Source) -> Result<Packet> {
        unsafe {
            let (mut data, mut frames, mut flags, mut qpc) = (std::ptr::null_mut(), 0, 0, 0);
            self.capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))?;
            let count = frames as usize * usize::from(CHANNELS);
            let samples = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                vec![0; count]
            } else {
                std::slice::from_raw_parts(data.cast::<i16>(), count).to_vec()
            };
            self.capture.ReleaseBuffer(frames)?;
            Ok(Packet {
                source,
                captured: qpc as i64,
                samples,
            })
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

/// Within this, ticks, a packet counts as on time after the one before it
/// from the same source: placed right after it, nothing added or trimmed.
const TOLERANCE: i64 = TICKS_PER_SECOND / 50;

/// The sources' sound mixed into one track, held from where the track
/// written so far ends until it is written out.
#[derive(Debug, Default)]
pub(super) struct Mixer {
    /// Frames written out so far.
    written: u64,
    /// Mixed samples from `written` on, the channels interleaved: wide, so
    /// the sources add up before they are clipped.
    pending: Vec<i32>,
    /// Where each source's sound so far ends, frames.
    ends: [u64; 2],
}

impl Mixer {
    /// Frames written out so far.
    pub(super) fn written(&self) -> u64 {
        self.written
    }

    /// Mix `samples` from `source`, captured at `time` (ticks on the
    /// recording's timeline): right after that source's last packet if on
    /// time, after silence if it came late, trimmed if early. Sound for
    /// what is written out already is left out.
    pub(super) fn add(&mut self, source: Source, time: i64, samples: &[i16]) {
        let channels = usize::from(CHANNELS);
        let frames = (samples.len() / channels) as u64;
        let end = &mut self.ends[source.index()];
        let gap = time - frames_to_ticks(*end);
        // Where what is kept begins, and how much of the start is trimmed.
        let (start, trim) = if gap > TOLERANCE {
            (ticks_to_frames(time), 0)
        } else if gap < -TOLERANCE {
            (*end, ticks_to_frames(-gap).min(frames))
        } else {
            (*end, 0)
        };
        *end = start + (frames - trim);
        // What falls on what is written out already is too late to mix.
        let late = self.written.saturating_sub(start).min(frames - trim);
        let from = (start + late - self.written) as usize * channels;
        let kept = &samples[(trim + late) as usize * channels..];
        if self.pending.len() < from + kept.len() {
            self.pending.resize(from + kept.len(), 0);
        }
        for (mixed, &sample) in self.pending[from..].iter_mut().zip(kept) {
            *mixed += i32::from(sample);
        }
    }

    /// The mix up to `time` (ticks), to write out from frame
    /// [`Mixer::written`]: silence where no source had sound. Empty if the
    /// track reaches `time` already.
    pub(super) fn take_until(&mut self, time: i64) -> Vec<i16> {
        let until = ticks_to_frames(time);
        if until <= self.written {
            return Vec::new();
        }
        let count = (until - self.written) as usize * usize::from(CHANNELS);
        if self.pending.len() < count {
            self.pending.resize(count, 0);
        }
        self.written = until;
        self.pending
            .drain(..count)
            .map(|sample| sample.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16)
            .collect()
    }
}

/// `frames` of sound at the track's rate, as 100 ns ticks.
pub(super) fn frames_to_ticks(frames: u64) -> i64 {
    (frames as i128 * TICKS_PER_SECOND as i128 / i128::from(RATE)) as i64
}

/// `ticks` (none if negative) as whole frames at the track's rate.
fn ticks_to_frames(ticks: i64) -> u64 {
    (ticks.max(0) as i128 * i128::from(RATE) / TICKS_PER_SECOND as i128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tenth of a second, frames and ticks.
    const TENTH: usize = RATE as usize / 10;
    const TENTH_TICKS: i64 = TICKS_PER_SECOND / 10;

    /// A tenth of a second of one sample value, both channels.
    fn tone(value: i16) -> Vec<i16> {
        vec![value; TENTH * usize::from(CHANNELS)]
    }

    #[test]
    fn packets_on_time_follow_one_another() {
        let mut mixer = Mixer::default();
        mixer.add(Source::System, 0, &tone(1));
        // A little late, or a little early: still right after.
        mixer.add(Source::System, TENTH_TICKS + 1_000, &tone(2));
        mixer.add(Source::System, 2 * TENTH_TICKS - 1_000, &tone(3));
        let out = mixer.take_until(3 * TENTH_TICKS);
        assert_eq!(out.len(), 3 * TENTH * 2);
        assert_eq!((out[0], out[TENTH * 2], out[2 * TENTH * 2]), (1, 2, 3));
        assert_eq!(mixer.written(), 3 * TENTH as u64);
    }

    #[test]
    fn a_gap_is_silent_and_an_overlap_is_trimmed() {
        let mut mixer = Mixer::default();
        mixer.add(Source::System, 0, &tone(1));
        // Nothing played for a tenth: silence, then the next.
        mixer.add(Source::System, 2 * TENTH_TICKS, &tone(2));
        // Half a tenth early: its first half is left out.
        mixer.add(Source::System, 3 * TENTH_TICKS - TENTH_TICKS / 2, &tone(3));
        let out = mixer.take_until(4 * TENTH_TICKS);
        let at = |tenths: f32| out[(tenths * TENTH as f32) as usize * 2];
        assert_eq!(
            (at(0.5), at(1.5), at(2.5), at(3.25), at(3.75)),
            (1, 0, 2, 3, 0)
        );
    }

    #[test]
    fn two_sources_add_up_and_clip() {
        let mut mixer = Mixer::default();
        mixer.add(Source::System, 0, &tone(1_000));
        mixer.add(Source::Microphone, 0, &tone(500));
        mixer.add(Source::System, TENTH_TICKS, &tone(30_000));
        mixer.add(Source::Microphone, TENTH_TICKS, &tone(30_000));
        let out = mixer.take_until(2 * TENTH_TICKS);
        assert_eq!((out[0], out[TENTH * 2]), (1_500, i16::MAX));
    }

    #[test]
    fn the_track_is_silent_where_no_source_has_sound_and_late_sound_is_left_out() {
        let mut mixer = Mixer::default();
        assert_eq!(
            mixer.take_until(TICKS_PER_SECOND),
            vec![0; RATE as usize * 2]
        );
        assert!(mixer.take_until(TICKS_PER_SECOND / 2).is_empty());
        // Sound for what is written out already: only what comes after.
        mixer.add(
            Source::Microphone,
            TICKS_PER_SECOND - TENTH_TICKS / 2,
            &tone(7),
        );
        let out = mixer.take_until(TICKS_PER_SECOND + TENTH_TICKS);
        assert_eq!((out[0], out[TENTH / 2 * 2], out[TENTH * 2 - 1]), (7, 0, 0));
    }
}
