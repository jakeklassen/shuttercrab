//! A recording's sound: what the speakers play, captured with Windows' loopback
//! (WASAPI) on the default output device, on a thread of its own.
//!
//! Windows converts it to the track's format (48 kHz, 16-bit stereo), whatever
//! the device runs at, and stamps each packet with the time of its first
//! sample on the same clock as the video frames (QPC). The recording thread
//! places the packets on the recording's timeline with a [`Soundtrack`]:
//! where a packet's time says, filling gaps with silence and trimming
//! overlaps, so the sound never drifts from the picture.

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
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
    Media::Audio::{
        AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
        AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioCaptureClient, IAudioClient,
        IMMDeviceEnumerator, MMDeviceEnumerator, WAVE_FORMAT_PCM, WAVEFORMATEX, eConsole, eRender,
    },
    System::{
        Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize},
        Threading::{CreateEventW, WaitForSingleObject},
    },
};

/// The track's sample rate, frames a second.
pub(super) const RATE: u32 = 48_000;

/// The track's channels: stereo.
pub(super) const CHANNELS: u16 = 2;

/// Sound captured from `captured` (QPC ticks, its first frame) on: 16-bit
/// samples, the channels interleaved.
pub(super) struct Packet {
    pub(super) captured: i64,
    pub(super) samples: Vec<i16>,
}

/// How much sound the device buffers, 100 ns ticks: room for the thread to
/// be late now and then.
const BUFFER: i64 = TICKS_PER_SECOND / 5;

/// How long the thread waits for sound before checking whether to stop.
const WAKE_MS: u32 = 100;

/// The loopback capture, running on its own thread until dropped.
pub(super) struct SystemSound {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SystemSound {
    /// Start capturing what the default output device plays, sending each
    /// packet to the recording thread as [`Event::Sound`]. Returns once the
    /// capture is running, or why it could not start.
    pub(super) fn start(events: Sender<Event>) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready, started) = std::sync::mpsc::channel::<Result<()>>();
        let stopping = stop.clone();
        let thread = std::thread::Builder::new()
            .name("shuttercrab-sound".into())
            .spawn(move || capture(&events, &stopping, ready))
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

impl Drop for SystemSound {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The capture thread: open the loopback, say whether it started, then pass
/// on packets until told to stop. A device that goes away (headphones
/// unplugged) is replaced by the new default device.
fn capture(events: &Sender<Event>, stop: &AtomicBool, ready: std::sync::mpsc::Sender<Result<()>>) {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let mut ready = Some(ready);
    while !stop.load(Ordering::Acquire) {
        let loopback = match Loopback::open() {
            Ok(loopback) => loopback,
            Err(e) => {
                match ready.take() {
                    Some(ready) => {
                        let _ = ready.send(Err(e));
                        break;
                    }
                    // Mid-recording: wait for a device, and try again.
                    None => log::warn!("no sound device to record: {e:#}"),
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
                continue;
            }
        };
        if let Some(ready) = ready.take() {
            let _ = ready.send(Ok(()));
        }
        match loopback.pass_on(events, stop) {
            Ok(()) => break,
            Err(e) => log::warn!("the sound device changed or went away: {e:#}; reopening"),
        }
    }
    if com {
        unsafe { CoUninitialize() };
    }
}

/// An open loopback on the default output device.
struct Loopback {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
}

impl Loopback {
    fn open() -> Result<Self> {
        unsafe {
            let devices: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .context("no audio device enumerator")?;
            let device = devices
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .context("no default output device")?;
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .context("could not open the output device")?;
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
            let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            client
                .Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER, 0, &format, None)
                .context("could not start the loopback")?;
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
                    Err(e.context("could not start the loopback"))
                }
            }
        }
    }

    /// Send every packet on until `stop`; an error means the device went
    /// away.
    fn pass_on(&self, events: &Sender<Event>, stop: &AtomicBool) -> Result<()> {
        while !stop.load(Ordering::Acquire) {
            let woken = unsafe { WaitForSingleObject(self.event, WAKE_MS) };
            if woken != WAIT_OBJECT_0 {
                continue;
            }
            while unsafe { self.capture.GetNextPacketSize() }? > 0 {
                let packet = self.next_packet()?;
                if events.send(Event::Sound(packet)).is_err() {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn next_packet(&self) -> Result<Packet> {
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
                captured: qpc as i64,
                samples,
            })
        }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

/// Where the sound written so far ends on the recording's timeline, and
/// where the next packet goes.
#[derive(Debug, Default)]
pub(super) struct Soundtrack {
    /// Frames written so far, silence included.
    written: u64,
}

/// What to write for a packet: silence before it, and how many of its
/// frames to leave out at its start.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Placement {
    pub(super) silence: u64,
    pub(super) skip: usize,
}

/// Within this, ticks, a packet counts as on time: no silence added, nothing
/// trimmed (sound and picture clocks agree to well within it).
const TOLERANCE: i64 = TICKS_PER_SECOND / 50;

impl Soundtrack {
    /// Frames written so far.
    pub(super) fn written(&self) -> u64 {
        self.written
    }

    /// Where the track ends, 100 ns ticks.
    pub(super) fn end(&self) -> i64 {
        frames_to_ticks(self.written)
    }

    /// Where a packet of `frames` timed `time` (ticks on the recording's
    /// timeline) goes: after silence if it starts later than the track
    /// ends, trimmed if it starts earlier. The track then ends after it.
    pub(super) fn place(&mut self, time: i64, frames: usize) -> Placement {
        let gap = time - self.end();
        let placement = if gap > TOLERANCE {
            Placement {
                silence: ticks_to_frames(gap),
                skip: 0,
            }
        } else if gap < -TOLERANCE {
            Placement {
                silence: 0,
                skip: (ticks_to_frames(-gap) as usize).min(frames),
            }
        } else {
            Placement {
                silence: 0,
                skip: 0,
            }
        };
        self.written += placement.silence + (frames - placement.skip) as u64;
        placement
    }

    /// Silence to write so the track reaches `time`: none if it does
    /// already.
    pub(super) fn silence_until(&mut self, time: i64) -> u64 {
        let missing = ticks_to_frames(time - self.end());
        self.written += missing;
        missing
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

    /// A tenth of a second of sound.
    const TENTH: usize = RATE as usize / 10;

    #[test]
    fn packets_on_time_follow_one_another() {
        let mut track = Soundtrack::default();
        let none = Placement {
            silence: 0,
            skip: 0,
        };
        assert_eq!(track.place(0, TENTH), none);
        // A little late, or a little early: still on time.
        assert_eq!(track.place(TICKS_PER_SECOND / 10 + 1_000, TENTH), none);
        assert_eq!(track.place(TICKS_PER_SECOND * 2 / 10 - 1_000, TENTH), none);
        assert_eq!(track.end(), TICKS_PER_SECOND * 3 / 10);
    }

    #[test]
    fn a_gap_is_filled_with_silence_and_an_overlap_trimmed() {
        let mut track = Soundtrack::default();
        track.place(0, TENTH);
        // Nothing played for a second: the next packet comes after silence.
        let late = track.place(TICKS_PER_SECOND * 11 / 10, TENTH);
        assert_eq!(late.silence, RATE as u64);
        assert_eq!(track.end(), TICKS_PER_SECOND * 12 / 10);
        // One that starts half a tenth before the track ends loses that much.
        let early = track.place(TICKS_PER_SECOND * 115 / 100, TENTH);
        assert_eq!(early.skip, TENTH / 2);
        assert_eq!(track.end(), TICKS_PER_SECOND * 125 / 100);
        // One wholly before the end is left out.
        assert_eq!(track.place(0, TENTH).skip, TENTH);
    }

    #[test]
    fn silence_keeps_the_track_up_with_the_picture() {
        let mut track = Soundtrack::default();
        assert_eq!(track.silence_until(TICKS_PER_SECOND), RATE as u64);
        // Already there: nothing more.
        assert_eq!(track.silence_until(TICKS_PER_SECOND / 2), 0);
        assert_eq!(track.end(), TICKS_PER_SECOND);
    }
}
