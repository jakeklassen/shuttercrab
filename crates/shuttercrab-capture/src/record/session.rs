//! One recording: the capture, the GPU conversion, and the frame loop that
//! hands frames to the encoder.

use super::{
    ANALYSE_EVERY, Event, HARDWARE_MIN_SIDE, HEARTBEAT, Interruption, MIN_SIDE, RecordOptions,
    RecordingSummary, TICKS_PER_SECOND,
    encoder::{Mp4Writer, Nv12},
    exposure::Exposure,
    fit::Fitter,
    qpc_ticks, recordable,
    sound::{self, Capture, Device, Mixer, Packet, Source},
    timing::Timer,
};
use crate::{
    color::Highlights,
    display,
    gpu::{Gpu, VideoConverter},
    screen::PhysicalRect,
    service::hmonitor,
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
        Foundation::HWND,
        Graphics::{
            Direct3D11::*,
            Dxgi::Common::*,
            Gdi::{HMONITOR, MONITOR_DEFAULTTONEAREST, MonitorFromWindow},
        },
        System::WinRT::{
            Direct3D11::IDirect3DDxgiInterfaceAccess,
            Graphics::Capture::IGraphicsCaptureItemInterop,
        },
        UI::WindowsAndMessaging::IsIconic,
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
    /// Each source's capture while it is on (the speakers, then the
    /// microphone), and their mix.
    captures: [Option<Capture>; 2],
    /// What following a recorded window needs; `None` for a display.
    window: Option<WindowTarget>,
    mixer: Mixer,
    /// How each source's sound arrived, for the log.
    arrivals: [Arrivals; 2],
    /// Where captures send their sound, to start one later.
    sounds: Sender<Event>,
}

/// A recorded window, followed wherever it goes.
struct WindowTarget {
    hwnd: HWND,
    /// Fits its picture into the frame once its size differs.
    fitter: Fitter,
    /// The size the frame pool was made for: the window's last size.
    pool_size: SizeInt32,
    /// The monitor it is on, whose white level the conversion uses.
    monitor: HMONITOR,
}

/// What a recording captures: a display, or one window.
struct Target<'a> {
    item: GraphicsCaptureItem,
    /// The monitor it starts on, for its GPU and white level.
    monitor: &'a display::Monitor,
    /// The size of the frames Windows sends.
    size: SizeInt32,
    /// What of each frame is recorded, which is also the video's size: for
    /// a window, all of the size it starts at.
    region: PhysicalRect,
    window: Option<HWND>,
}

impl<'a> Target<'a> {
    fn new(options: &RecordOptions, monitors: &'a [display::Monitor]) -> Result<Self> {
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let find = |handle: HMONITOR| {
            monitors
                .iter()
                .find(|m| m.hmonitor == handle)
                .context("the monitor is no longer attached")
        };
        if let Some(handle) = options.window {
            let hwnd = HWND(handle as _);
            let monitor = find(unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) })?;
            let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd) }
                .context("this window cannot be recorded")?;
            let size = item.Size()?;
            let side = |s: i32| (s.max(0) as u32).max(MIN_SIDE) & !1;
            return Ok(Self {
                item,
                monitor,
                size,
                region: PhysicalRect::new(0, 0, side(size.Width), side(size.Height)),
                window: Some(hwnd),
            });
        }
        let monitor = find(hmonitor(options.monitor))?;
        let item: GraphicsCaptureItem =
            unsafe { interop.CreateForMonitor(hmonitor(options.monitor)) }
                .context("CreateForMonitor failed")?;
        let (width, height) = (monitor.bounds.width, monitor.bounds.height);
        Ok(Self {
            item,
            monitor,
            size: SizeInt32 {
                Width: width as i32,
                Height: height as i32,
            },
            region: recordable(options.region, width, height),
            window: None,
        })
    }
}

/// How one source's sound arrived, in ticks: to tell from the log whether
/// sound is missing because it came late.
#[derive(Clone, Copy, Debug, Default)]
struct Arrivals {
    /// The first packet's time on the timeline, and how long after it was
    /// captured it arrived.
    first: Option<(i64, i64)>,
    /// The longest any packet took to arrive.
    slowest: i64,
}

impl Session {
    pub(super) fn new(options: &RecordOptions, frames: Sender<Event>) -> Result<Self> {
        ensure!(
            matches!(options.fps, 1..=120),
            "a frame rate of {} is not supported",
            options.fps
        );
        ensure!(
            GraphicsCaptureSession::IsSupported()?,
            "Windows.Graphics.Capture is not available"
        );
        let monitors = display::enumerate()?;
        let target = Target::new(options, &monitors)?;
        let monitor = target.monitor;
        let white_scale = monitor.white_scale()?;
        let region = target.region;
        ensure!(!region.is_empty(), "the region is empty");

        let gpu = Gpu::video(Some(&monitor.adapter))?;
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
            options.has_sound(),
        )?;

        let window = match target.window {
            Some(hwnd) => Some(WindowTarget {
                hwnd,
                fitter: Fitter::new(&gpu, &crop)?,
                pool_size: target.size,
                monitor: monitor.hmonitor,
            }),
            None => None,
        };
        let (item, size) = (target.item, target.size);

        // The capture: FP16, two buffers, the pointer only if asked.
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
        // Windows closes the item when its display goes away (frames just
        // stop otherwise), or its window closes.
        let is_window = window.is_some();
        let closed_token = item.Closed(&TypedEventHandler::new(move |_, _| {
            let _ = closed.send(if is_window {
                Event::WindowClosed
            } else {
                Event::DisplayGone
            });
            Ok(())
        }))?;
        let capture = pool.CreateCaptureSession(&item)?;
        capture.SetIsCursorCaptureEnabled(options.include_cursor)?;
        let _ = capture.SetIsBorderRequired(false);
        capture.StartCapture().context("StartCapture failed")?;
        let captures = [
            start_sound(Source::System, options, &sounds),
            start_sound(Source::Microphone, options, &sounds),
        ];
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
            captures,
            window,
            mixer: Mixer::default(),
            arrivals: [Arrivals::default(); 2],
            sounds,
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
                Event::WindowClosed => {
                    log::info!("recording ended: the window was closed");
                    return Some(Interruption::WindowClosed);
                }
                Event::DisplayChanged => self.reread_white_scale(),
                Event::Sound(packet) => self.place_sound(&packet, timeline),
                Event::SetSound(source, on) => self.set_sound(source, on),
                Event::SoundProcess(process) => self.set_sound_process(process),
                Event::ShowCursor(show) => {
                    let show = show && self.options.include_cursor;
                    if let Err(e) = self.capture.SetIsCursorCaptureEnabled(show) {
                        log::warn!("could not show or hide the pointer: {e}");
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

    /// Mix a packet of sound in where its time puts it on the timeline.
    /// Sound while paused is left out, as frames are.
    fn place_sound(&mut self, packet: &Packet, timeline: &Timeline) {
        if !timeline.is_paused() {
            let time = timeline.sound_time(packet.captured);
            let arrivals = &mut self.arrivals[packet.source as usize];
            let delay = qpc_ticks() - packet.captured;
            arrivals.first.get_or_insert((time, delay));
            arrivals.slowest = arrivals.slowest.max(delay);
            self.mixer.add(packet.source, time, &packet.samples);
        }
    }

    /// Switch `source` on or off mid-recording. A source that cannot
    /// start leaves its part of the track silent.
    fn set_sound(&mut self, source: Source, on: bool) {
        let slot = &mut self.captures[source as usize];
        match (on, slot.is_some()) {
            (true, false) => {
                let device = device_for(source, &self.options);
                *slot = Capture::start(source, device, self.sounds.clone())
                    .inspect_err(|e| log::warn!("could not switch on {source:?}: {e:#}"))
                    .ok();
            }
            (false, true) => *slot = None,
            _ => {}
        }
        log::info!("{source:?} {}", if on { "on" } else { "off" });
    }

    /// Record only a process's share of the speakers' sound, or all of it:
    /// a running capture is replaced by one listening as asked. The new one
    /// starts before the old one stops: opening takes a fifth of a second,
    /// which would otherwise be a hole in the sound.
    fn set_sound_process(&mut self, process: Option<u32>) {
        if self.options.sound_process == process {
            return;
        }
        self.options.sound_process = process;
        log::info!(
            "system sound: {}",
            if process.is_some() {
                "the app's"
            } else {
                "all"
            }
        );
        let slot = &mut self.captures[Source::System as usize];
        if slot.is_none() {
            return;
        }
        let device = device_for(Source::System, &self.options);
        match Capture::start(Source::System, device, self.sounds.clone()) {
            // The old capture stops as it is replaced.
            Ok(capture) => *slot = Some(capture),
            // Better the sound as it was than none.
            Err(e) => log::warn!("could not switch the system sound: {e:#}"),
        }
    }

    /// Write the mix out up to `time` on the timeline: silence where no
    /// source had sound.
    fn write_mix_until(&mut self, time: i64) -> Result<()> {
        let first = self.mixer.written();
        let samples = self.mixer.take_until(time);
        self.writer
            .write_sound(&samples, sound::frames_to_ticks(first))
    }

    /// Keep the track up with the picture, a little behind now: sound
    /// arrives late, and nothing playing sends none (the writer holds
    /// frames back for the track otherwise). Not while paused.
    fn keep_sound_up(&mut self, timeline: &Timeline) -> Result<()> {
        if !self.writer.has_sound() || timeline.is_paused() {
            return Ok(());
        }
        self.write_mix_until(timeline.now() - SOUND_LAG)
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
            // A minimised window's frames are junk (one flashed a solid
            // colour as it minimised). The app pauses the recording, but
            // only once Windows has told it.
            if let Some(window) = &self.window
                && unsafe { IsIconic(window.hwnd) }.as_bool()
            {
                frame.Close()?;
                continue;
            }
            self.follow_window(&frame)?;
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
        self.copy_frame(frame)?;
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
        if timeline.last_written.is_none() {
            log::info!("first frame at {} ms", time / (TICKS_PER_SECOND / 1000));
        }
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
        self.captures = [None, None];
        self.log_arrivals();
        if interrupted.is_none() && self.writer.has_sound() {
            self.write_mix_until(duration_ticks(duration))
                .map_err(why)?;
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

    /// Say how each source's sound arrived.
    fn log_arrivals(&self) {
        let ms = |ticks: i64| ticks / (TICKS_PER_SECOND / 1000);
        for source in [Source::System, Source::Microphone] {
            let arrivals = self.arrivals[source as usize];
            let Some((time, delay)) = arrivals.first else {
                continue;
            };
            log::info!(
                "{source:?} sound: first at {} ms, arrived {} ms after capture; slowest {} ms; {} ms too late to mix",
                ms(time),
                ms(delay),
                ms(arrivals.slowest),
                ms(self.mixer.late(source)),
            );
        }
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

    /// A recorded window that changed size gets a frame pool of its new
    /// size, so its next frames hold all of it; one that moved to another
    /// monitor takes that monitor's white level.
    fn follow_window(&mut self, frame: &Direct3D11CaptureFrame) -> Result<()> {
        let Some(window) = self.window.as_mut() else {
            return Ok(());
        };
        let content = frame.ContentSize()?;
        if content.Width > 0 && content.Height > 0 && content != window.pool_size {
            self.pool.Recreate(
                &self.gpu.winrt_device()?,
                DirectXPixelFormat::R16G16B16A16Float,
                2,
                content,
            )?;
            window.pool_size = content;
            log::debug!("the window is now {}×{}", content.Width, content.Height);
        }
        let monitor = unsafe { MonitorFromWindow(window.hwnd, MONITOR_DEFAULTTONEAREST) };
        if monitor != window.monitor {
            window.monitor = monitor;
            log::info!("the window moved to another display");
            self.reread_white_scale();
        }
        Ok(())
    }

    /// Copy what is recorded out of the captured frame: the region of a
    /// display; a window as it is, or fitted into the frame once its size
    /// differs from the size it started at.
    fn copy_frame(&mut self, frame: &Direct3D11CaptureFrame) -> Result<()> {
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        let source: ID3D11Texture2D = unsafe { access.GetInterface()? };
        let r = self.region;
        let content = frame.ContentSize()?;
        if let Some(window) = self.window.as_mut() {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { source.GetDesc(&mut desc) };
            // A frame from before the pool was remade is the old size.
            let size = (
                (content.Width.max(1) as u32).min(desc.Width),
                (content.Height.max(1) as u32).min(desc.Height),
            );
            // The frame is the starting size rounded down to even: one
            // pixel more is still the same size.
            let same = |side: u32, frame: u32| side == frame || side == frame + 1;
            if !(same(size.0, r.width) && same(size.1, r.height)) {
                return window.fitter.fit(&self.gpu, &source, size);
            }
        }
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
            let handle = match &self.window {
                Some(window) => window.monitor,
                None => hmonitor(self.options.monitor),
            };
            let monitor = monitors
                .iter()
                .find(|m| m.hmonitor == handle)
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
    // Written by a copy, or by the window fit's shader.
    let crop = gpu.texture(
        w,
        h,
        DXGI_FORMAT_R16G16B16A16_FLOAT,
        D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_UNORDERED_ACCESS,
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

/// Where `source`'s sound comes from, as `options` say.
fn device_for(source: Source, options: &RecordOptions) -> Device {
    match source {
        Source::System => options
            .sound_process
            .map_or(Device::Default, Device::Process),
        Source::Microphone => options
            .microphone_device
            .clone()
            .map_or(Device::Default, Device::Microphone),
    }
}

/// Start capturing `source` if `options` switch it on. Without a device
/// to record, its part of the track stays silent: the rest still records.
fn start_sound(source: Source, options: &RecordOptions, events: &Sender<Event>) -> Option<Capture> {
    let on = match source {
        Source::System => options.system_sound,
        Source::Microphone => options.microphone,
    };
    if !on {
        return None;
    }
    Capture::start(source, device_for(source, options), events.clone())
        .inspect_err(|e| log::warn!("recording without {source:?}: {e:#}"))
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
