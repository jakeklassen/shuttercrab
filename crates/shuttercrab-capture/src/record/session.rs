//! One recording: the capture, the GPU conversion, and the frame loop that
//! hands frames to the encoder.

use super::{
    ANALYSE_EVERY, Event, HARDWARE_MIN_SIDE, HEARTBEAT, Interruption, RecordOptions,
    RecordingSummary, TICKS_PER_SECOND,
    encoder::{Mp4Writer, Nv12},
    exposure::Exposure,
    qpc_ticks, recordable,
    sound::{self, Packet, Soundtrack, SystemSound},
    timing::Timer,
};
use crate::{
    color::Highlights,
    display,
    gpu::{Gpu, VideoConverter},
    service::{PhysicalRect, hmonitor},
};
use anyhow::{Context, Result, ensure};
use std::{
    sync::mpsc::{Receiver, RecvTimeoutError, Sender},
    time::{Duration, Instant},
};
use windows::{
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{
            Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
            GraphicsCaptureSession,
        },
        DirectX::DirectXPixelFormat,
        SizeInt32,
    },
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1},
            Direct3D11::*,
            Dxgi::{Common::*, IDXGIAdapter},
        },
        System::WinRT::{
            Direct3D11::IDirect3DDxgiInterfaceAccess,
            Graphics::Capture::IGraphicsCaptureItemInterop,
        },
    },
    core::{Interface, factory},
};

/// One recording's capture, GPU work and encoder.
pub(super) struct Session {
    options: RecordOptions,
    gpu: Gpu,
    converter: VideoConverter,
    white_scale: f32,
    region: PhysicalRect,
    pool: Direct3D11CaptureFramePool,
    capture: GraphicsCaptureSession,
    /// The display being captured, and its Closed registration.
    item: GraphicsCaptureItem,
    closed_token: i64,
    /// The FrameArrived registration, until the capture is stopped.
    frame_token: Option<i64>,
    crop: ID3D11Texture2D,
    sdr: ID3D11Texture2D,
    rgba: ID3D11Texture2D,
    nv12: Nv12,
    writer: Mp4Writer,
    exposure: Exposure,
    /// The sound capture, while it runs, and where its track ends.
    sound: Option<SystemSound>,
    track: Soundtrack,
}

impl Session {
    pub(super) fn new(options: &RecordOptions, frames: Sender<Event>) -> Result<Self> {
        ensure!(
            matches!(options.fps, 1..=120),
            "a frame rate of {} is not supported",
            options.fps
        );
        let monitors = display::enumerate()?;
        let monitor = monitors
            .iter()
            .find(|m| m.hmonitor == hmonitor(options.monitor))
            .context("the monitor is no longer attached")?;
        let white_scale = monitor.white_scale()?;
        let (width, height) = (monitor.bounds.width, monitor.bounds.height);
        let region = recordable(options.region, width, height);
        ensure!(!region.is_empty(), "the region is empty");

        let gpu = video_gpu(&monitor.adapter)?;
        let (w, h) = (region.width, region.height);
        let [crop, sdr, rgba] = work_textures(&gpu, w, h)?;
        let converter = VideoConverter::new(&gpu, &crop, &sdr, ANALYSE_EVERY)?;
        let nv12 = Nv12::new(&gpu, &rgba, w, h, options.fps)?;
        let hardware = w.min(h) >= HARDWARE_MIN_SIDE;
        let writer = Mp4Writer::new(
            &gpu,
            &options.path,
            (w, h, options.fps),
            hardware,
            options.system_sound,
        )?;

        // The capture: FP16, two buffers, the pointer only if asked.
        ensure!(
            GraphicsCaptureSession::IsSupported()?,
            "Windows.Graphics.Capture is not available"
        );
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem =
            unsafe { interop.CreateForMonitor(hmonitor(options.monitor)) }
                .context("CreateForMonitor failed")?;
        let size = SizeInt32 {
            Width: monitor.bounds.width as i32,
            Height: monitor.bounds.height as i32,
        };
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &gpu.winrt_device()?,
            DirectXPixelFormat::R16G16B16A16Float,
            2,
            size,
        )?;
        let closed = frames.clone();
        let sounds = frames.clone();
        let frame_token = pool.FrameArrived(&TypedEventHandler::new(move |_, _| {
            let _ = frames.send(Event::Frame);
            Ok(())
        }))?;
        // Windows closes the item when its display goes away; frames just
        // stop otherwise.
        let closed_token = item.Closed(&TypedEventHandler::new(move |_, _| {
            let _ = closed.send(Event::DisplayGone);
            Ok(())
        }))?;
        let capture = pool.CreateCaptureSession(&item)?;
        capture.SetIsCursorCaptureEnabled(options.include_cursor)?;
        let _ = capture.SetIsBorderRequired(false);
        capture.StartCapture().context("StartCapture failed")?;
        let sound = start_sound(options, sounds);
        Ok(Self {
            options: options.clone(),
            gpu,
            converter,
            white_scale,
            region,
            pool,
            capture,
            item,
            closed_token,
            frame_token: Some(frame_token),
            crop,
            sdr,
            rgba,
            nv12,
            writer,
            exposure: Exposure {
                value: None,
                last: 0,
            },
            sound,
            track: Soundtrack::default(),
        })
    }

    /// Record until stopped, then finish the file. A failure ends the
    /// recording, not the file: what was written is still finished.
    pub(super) fn run(&mut self, inbox: &Receiver<Event>) -> Result<RecordingSummary> {
        let mut timeline = Timeline::start(self.options.fps);
        let mut counts = Counts::default();
        // Measuring costs a few queries per frame and never waits.
        let mut timer = Timer::new(&self.gpu)
            .inspect_err(|e| log::debug!("no frame timing: {e:#}"))
            .ok();
        let interrupted = self.record_until_stopped(inbox, &mut timeline, &mut counts, &mut timer);
        self.finish(timeline, counts, timer, interrupted)
    }

    /// Handle events until the recording is stopped or interrupted; returns
    /// why it was interrupted, if it was.
    fn record_until_stopped(
        &mut self,
        inbox: &Receiver<Event>,
        timeline: &mut Timeline,
        counts: &mut Counts,
        timer: &mut Option<Timer>,
    ) -> Option<Interruption> {
        let interrupted = |e: anyhow::Error| {
            log::error!("recording interrupted: {e:#}");
            Some(Interruption::from_error(&e))
        };
        loop {
            if let Err(e) = self.keep_sound_up(timeline) {
                return interrupted(e);
            }
            // Sleep until the next still-screen repeat is due, or for good
            // while paused or before the first frame: frames and requests
            // wake the loop themselves.
            let received = match timeline.until_repeat_due() {
                Some(wait) => inbox.recv_timeout(wait),
                None => inbox.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            let event = match received {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => {
                    if let Err(e) = self.repeat_last_frame(timeline, counts) {
                        return interrupted(e);
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => Event::Stop,
            };
            match event {
                Event::Pause => timeline.pause(),
                Event::Resume => timeline.resume(),
                Event::Stop => return None,
                Event::DisplayGone => {
                    log::warn!("recording interrupted: the display went away");
                    return Some(Interruption::DisplayGone);
                }
                Event::DisplayChanged => self.reread_white_scale(),
                Event::Sound(packet) => {
                    if let Err(e) = self.place_sound(packet, timeline) {
                        return interrupted(e);
                    }
                }
                Event::Frame => {
                    if let Err(e) = self.take_ready_frames(timeline, counts, timer) {
                        return interrupted(e);
                    }
                }
            }
        }
    }

    /// Write a packet of sound where its time puts it on the timeline,
    /// after silence if it came late, trimmed if early. Sound while paused
    /// is left out, as frames are.
    fn place_sound(&mut self, packet: Packet, timeline: &Timeline) -> Result<()> {
        if timeline.is_paused() {
            return Ok(());
        }
        let frames = packet.samples.len() / usize::from(sound::CHANNELS);
        let first = self.track.written();
        let placement = self
            .track
            .place(timeline.sound_time(packet.captured), frames);
        self.write_silence(first, placement.silence)?;
        let at = first + placement.silence;
        let kept = &packet.samples[placement.skip * usize::from(sound::CHANNELS)..];
        self.writer.write_sound(kept, sound::frames_to_ticks(at))
    }

    /// Nothing playing sends no sound: fill the track with silence up to
    /// a little before now, so it keeps up with the picture (the writer
    /// holds frames back for the track otherwise). Not while paused.
    fn keep_sound_up(&mut self, timeline: &Timeline) -> Result<()> {
        if self.sound.is_none() || timeline.is_paused() {
            return Ok(());
        }
        let first = self.track.written();
        let missing = self.track.silence_until(timeline.now() - SOUND_LAG);
        self.write_silence(first, missing)
    }

    /// Write `frames` of silence from frame `first` of the track.
    fn write_silence(&self, first: u64, frames: u64) -> Result<()> {
        let chunk = u64::from(sound::RATE) / 10;
        let mut done = 0;
        while done < frames {
            let count = chunk.min(frames - done);
            let zeros = vec![0; count as usize * usize::from(sound::CHANNELS)];
            self.writer
                .write_sound(&zeros, sound::frames_to_ticks(first + done))?;
            done += count;
        }
        Ok(())
    }

    /// A still screen delivers no frames: repeat the last one every second,
    /// so the video can be seeked and edited.
    fn repeat_last_frame(&mut self, timeline: &mut Timeline, counts: &mut Counts) -> Result<()> {
        if timeline.is_paused() {
            return Ok(());
        }
        let Some((previous, slot)) = timeline.last_written else {
            return Ok(());
        };
        let now = timeline.now();
        if now - previous >= HEARTBEAT {
            self.writer.write(&self.nv12, slot, now, timeline.period)?;
            timeline.last_written = Some((now, slot));
            counts.frames += 1;
        }
        Ok(())
    }

    /// Take every frame that is ready (the pool holds two), and encode the
    /// ones the recording keeps.
    fn take_ready_frames(
        &mut self,
        timeline: &mut Timeline,
        counts: &mut Counts,
        timer: &mut Option<Timer>,
    ) -> Result<()> {
        while let Some(frame) = next_frame(&self.pool)? {
            let captured = frame.SystemRelativeTime()?.Duration;
            if timeline.is_paused() {
                frame.Close()?;
                continue;
            }
            let time = timeline.output_time(captured);
            if timeline.too_soon(time) {
                counts.skipped_rate += 1;
                frame.Close()?;
                continue;
            }
            // Before the copy: a frame that cannot be encoded costs nothing.
            let Some(slot) = self.nv12.free_slot() else {
                counts.dropped_busy += 1;
                frame.Close()?;
                continue;
            };
            self.encode_frame(&frame, time, slot, timeline, timer)?;
            counts.frames += 1;
        }
        Ok(())
    }

    /// Copy, convert and hand one captured frame to the encoder at `time`,
    /// timing each stage if `timer` is measuring.
    fn encode_frame(
        &mut self,
        frame: &Direct3D11CaptureFrame,
        time: i64,
        slot: usize,
        timeline: &mut Timeline,
        timer: &mut Option<Timer>,
    ) -> Result<()> {
        let context = self.gpu.context.clone();
        let measured = timer.as_mut().map(|t| t.begin(&context));
        let stamp = |timer: &Option<Timer>, stage| {
            if let (Some(t), Some(i)) = (timer, measured) {
                t.stamp(&context, i, stage);
            }
        };
        self.copy_region(frame)?;
        frame.Close()?;
        stamp(timer, 1);
        let started = Instant::now();
        self.convert(time)?;
        let cpu_convert = started.elapsed();
        stamp(timer, 2);
        self.nv12.convert(&self.gpu, slot)?;
        // Before the hand-off: the encoder works on the same queue from its
        // own thread.
        stamp(timer, 3);
        let time = timeline
            .last_written
            .map_or(time, |(previous, _)| time.max(previous + 1));
        let started = Instant::now();
        self.writer.write(&self.nv12, slot, time, timeline.period)?;
        if let (Some(t), Some(i)) = (timer.as_mut(), measured) {
            t.end(&context, i, cpu_convert, started.elapsed());
        }
        timeline.last_written = Some((time, slot));
        Ok(())
    }

    /// Repeat the last frame at the stop, so the video lasts until then, and
    /// finish the file.
    fn finish(
        &mut self,
        mut timeline: Timeline,
        mut counts: Counts,
        timer: Option<Timer>,
        interrupted: Option<Interruption>,
    ) -> Result<RecordingSummary> {
        timeline.resume();
        let period = timeline.period;
        let why = |e: anyhow::Error| match &interrupted {
            Some(i) => e.context(format!("after the recording was interrupted ({i:?})")),
            None => e,
        };
        let end = timeline.now();
        let duration = match timeline.last_written {
            Some((time, slot)) => {
                let end = end.max(time + period);
                // Not after a failure: the encoder may be what failed.
                if end - period > time && interrupted.is_none() {
                    self.writer
                        .write(&self.nv12, slot, end - period, period)
                        .map_err(why)?;
                    counts.frames += 1;
                }
                let end = if interrupted.is_some() {
                    time + period
                } else {
                    end
                };
                ticks_to_duration(end)
            }
            None => return Err(why(anyhow::anyhow!("no frame was captured"))),
        };
        self.stop_capture();
        // The sound lasts as long as the picture.
        self.sound = None;
        if interrupted.is_none() && self.writer.has_sound() {
            let first = self.track.written();
            let missing = self.track.silence_until(duration_ticks(duration));
            self.write_silence(first, missing).map_err(why)?;
        }
        let timing = timer.map(|t| t.finish(&self.gpu.context));
        let hardware_encoder = self.writer.finish().map_err(why)?;
        Ok(RecordingSummary {
            timing,
            path: self.options.path.clone(),
            width: self.region.width,
            height: self.region.height,
            frames: counts.frames,
            dropped_busy: counts.dropped_busy,
            skipped_rate: counts.skipped_rate,
            duration,
            paused: ticks_to_duration(timeline.paused_ticks),
            hardware_encoder,
            sound: self.writer.has_sound(),
            interrupted,
        })
    }

    /// Stop capturing, once: removing the FrameArrived handler a second
    /// time with the same token makes Windows end the process.
    fn stop_capture(&mut self) {
        if let Some(token) = self.frame_token.take() {
            let _ = self.item.RemoveClosed(self.closed_token);
            let _ = self.pool.RemoveFrameArrived(token);
            let _ = self.capture.Close();
            let _ = self.pool.Close();
        }
    }

    /// Copy the region out of the captured frame.
    fn copy_region(
        &self,
        frame: &windows::Graphics::Capture::Direct3D11CaptureFrame,
    ) -> Result<()> {
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        let source: ID3D11Texture2D = unsafe { access.GetInterface()? };
        let r = self.region;
        let content = frame.ContentSize()?;
        ensure!(
            (r.x as u32 + r.width) as i32 <= content.Width
                && (r.y as u32 + r.height) as i32 <= content.Height,
            "the monitor changed size during the recording"
        );
        let area = D3D11_BOX {
            left: r.x as u32,
            top: r.y as u32,
            front: 0,
            right: r.x as u32 + r.width,
            bottom: r.y as u32 + r.height,
            back: 1,
        };
        unsafe {
            self.gpu
                .context
                .CopySubresourceRegion(&self.crop, 0, 0, 0, 0, &source, 0, Some(&area))
        };
        Ok(())
    }

    /// Read the display's white level again after a display change.
    fn reread_white_scale(&mut self) {
        let found = display::enumerate().and_then(|monitors| {
            let monitor = monitors
                .iter()
                .find(|m| m.hmonitor == hmonitor(self.options.monitor))
                .context("the monitor is no longer attached")?;
            monitor.white_scale()
        });
        match found {
            Ok(scale) if scale != self.white_scale => {
                log::info!(
                    "the display changed: SDR white is now {scale} (was {})",
                    self.white_scale
                );
                self.white_scale = scale;
                // Start the exposure afresh rather than easing from a
                // level measured at the old white.
                self.exposure = Exposure {
                    value: None,
                    last: 0,
                };
            }
            Ok(_) => log::debug!("the display changed; its white level did not"),
            Err(e) => log::warn!("could not read the display's white level again: {e:#}"),
        }
    }

    /// HDR → SDR into `rgba`, with steady exposure.
    fn convert(&mut self, time: i64) -> Result<()> {
        let exposure = &mut self.exposure;
        self.converter.convert(
            &self.gpu,
            self.white_scale,
            Highlights::Tonemap,
            |analysis| exposure.apply(analysis, time),
        )?;
        unsafe { self.gpu.context.CopyResource(&self.rgba, &self.sdr) };
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop_capture();
    }
}

/// A frame's working textures, `w`×`h`: the crop (FP16, as captured), the
/// SDR conversion's output, and the RGBA the NV12 conversion reads.
fn work_textures(gpu: &Gpu, w: u32, h: u32) -> Result<[ID3D11Texture2D; 3]> {
    let crop = gpu.texture(
        w,
        h,
        DXGI_FORMAT_R16G16B16A16_FLOAT,
        D3D11_BIND_SHADER_RESOURCE,
        None,
    )?;
    let sdr = gpu.texture(
        w,
        h,
        DXGI_FORMAT_R8G8B8A8_TYPELESS,
        D3D11_BIND_UNORDERED_ACCESS | D3D11_BIND_SHADER_RESOURCE,
        None,
    )?;
    let rgba = gpu.texture(
        w,
        h,
        DXGI_FORMAT_R8G8B8A8_UNORM,
        D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
        None,
    )?;
    Ok([crop, sdr, rgba])
}

/// Start capturing what the speakers play, if `options` ask for it. Without
/// a device to record, the track stays silent: the picture still records.
fn start_sound(options: &RecordOptions, events: Sender<Event>) -> Option<SystemSound> {
    if !options.system_sound {
        return None;
    }
    SystemSound::start(events)
        .inspect_err(|e| log::warn!("recording without sound: {e:#}"))
        .ok()
}

fn next_frame(
    pool: &Direct3D11CaptureFramePool,
) -> Result<Option<windows::Graphics::Capture::Direct3D11CaptureFrame>> {
    match pool.TryGetNextFrame() {
        Ok(frame) => Ok(Some(frame)),
        // An empty pool returns a null frame, which windows-rs surfaces as an
        // error carrying a success code.
        Err(e) if e.code().is_ok() => Ok(None),
        Err(e) => Err(e).context("TryGetNextFrame failed"),
    }
}

/// A hardware device on the monitor's adapter with the video APIs enabled.
fn video_gpu(adapter: &windows::Win32::Graphics::Dxgi::IDXGIAdapter1) -> Result<Gpu> {
    let adapter: IDXGIAdapter = adapter.cast()?;
    let (mut device, mut context) = (None, None);
    unsafe {
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .context("D3D11CreateDevice (video) failed")?;
    }
    Gpu::from_device(device.context("no device")?, context.context("no context")?)
}

/// The recording's clock: Windows' capture time in 100 ns ticks (QPC), with
/// paused time taken out, starting at zero when recording starts. Frames,
/// pauses and the stop are all measured on it.
struct Timeline {
    /// When recording started, QPC ticks.
    origin: i64,
    /// One frame at the recording's rate, ticks.
    period: i64,
    /// While paused: when the pause began, QPC ticks.
    paused_since: Option<i64>,
    /// Time spent paused so far, ticks.
    paused_ticks: i64,
    /// The last frame written: its time on this clock, and the NV12 slot
    /// that holds it (to repeat it on a still screen and at the stop).
    last_written: Option<(i64, usize)>,
}

impl Timeline {
    fn start(fps: u32) -> Self {
        Self {
            origin: qpc_ticks(),
            period: TICKS_PER_SECOND / fps as i64,
            paused_since: None,
            paused_ticks: 0,
            last_written: None,
        }
    }

    fn is_paused(&self) -> bool {
        self.paused_since.is_some()
    }

    fn pause(&mut self) {
        self.paused_since.get_or_insert_with(qpc_ticks);
    }

    fn resume(&mut self) {
        if let Some(since) = self.paused_since.take() {
            self.paused_ticks += qpc_ticks() - since;
        }
    }

    /// Now, on this clock.
    fn now(&self) -> i64 {
        qpc_ticks() - self.origin - self.paused_ticks
    }

    /// When sound captured at `captured` (QPC ticks) falls on this clock:
    /// before the start if it was captured before recording began.
    fn sound_time(&self, captured: i64) -> i64 {
        captured - self.origin - self.paused_ticks
    }

    /// When a frame captured at `captured` (QPC ticks) falls on this clock.
    fn output_time(&self, captured: i64) -> i64 {
        (captured - self.origin - self.paused_ticks).max(0)
    }

    /// Whether a frame at `time` comes too soon after the last one written
    /// for the frame rate. Nine tenths of a period counts as on time, so
    /// frames that arrive with a little jitter are kept.
    fn too_soon(&self, time: i64) -> bool {
        self.last_written
            .is_some_and(|(previous, _)| time < previous + self.period * 9 / 10)
    }

    /// How long until the last frame is due to be repeated (a still screen
    /// delivers none); `None` to wait for events alone, while paused or
    /// before the first frame.
    fn until_repeat_due(&self) -> Option<Duration> {
        match (self.paused_since, self.last_written) {
            (None, Some((previous, _))) => {
                let due = previous + HEARTBEAT - self.now();
                Some(ticks_to_duration(due.max(10_000)))
            }
            _ => None,
        }
    }
}

/// What a recording did with the frames Windows delivered.
#[derive(Default)]
struct Counts {
    /// Written to the file, repeats included.
    frames: u64,
    /// Dropped because every encoder texture was busy.
    dropped_busy: u64,
    /// Skipped because they came faster than the frame rate.
    skipped_rate: u64,
}

/// How far behind now the sound track is kept, 100 ns ticks: sound arrives
/// in packets a few hundredths of a second after it plays.
const SOUND_LAG: i64 = TICKS_PER_SECOND / 5;

/// A `Duration` as 100 ns ticks.
fn duration_ticks(duration: Duration) -> i64 {
    (duration.as_nanos() / 100) as i64
}

/// 100 ns ticks as a `Duration` (none if negative).
fn ticks_to_duration(ticks: i64) -> Duration {
    Duration::from_nanos(ticks.max(0) as u64 * 100)
}
