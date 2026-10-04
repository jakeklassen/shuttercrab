//! One recording: the capture, the GPU conversion, and the frame loop that
//! hands frames to the encoder.

use super::{
    ANALYSE_EVERY, Event, HARDWARE_MIN_SIDE, HEARTBEAT, Interruption, RecordOptions,
    RecordingSummary, TICKS_PER_SECOND,
    encoder::{Mp4Writer, Nv12},
    exposure::Exposure,
    qpc_ticks, recordable,
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
    sync::mpsc::{Receiver, Sender},
    time::{Duration, Instant},
};
use windows::{
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
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
        let converter = VideoConverter::new(&gpu, &crop, &sdr, ANALYSE_EVERY)?;
        let nv12 = Nv12::new(&gpu, &rgba, w, h, options.fps)?;
        let hardware = w.min(h) >= HARDWARE_MIN_SIDE;
        let writer = Mp4Writer::new(&gpu, &options.path, w, h, options.fps, hardware)?;

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
        })
    }

    pub(super) fn run(&mut self, inbox: &Receiver<Event>) -> Result<RecordingSummary> {
        let period = TICKS_PER_SECOND / self.options.fps as i64;
        // One clock throughout: frames carry their capture time in QPC
        // ticks (100 ns), and pauses and the stop are measured on it too.
        // The output timeline starts when recording starts.
        let origin = qpc_ticks();
        let mut paused_since: Option<i64> = None;
        let mut paused: i64 = 0;
        let mut last: Option<(i64, usize)> = None;
        let (mut frames, mut dropped_busy, mut skipped_rate) = (0u64, 0u64, 0u64);
        // Measuring costs a few queries per frame and never waits.
        let mut timer = Timer::new(&self.gpu)
            .inspect_err(|e| log::debug!("no frame timing: {e:#}"))
            .ok();
        // A failure ends the loop, not the recording: what was written is
        // still finished into a file.
        let mut interrupted = None;
        loop {
            // Sleep until the next still-screen repeat is due, or for good
            // while paused or before the first frame: frames and requests
            // wake the loop themselves.
            let next = match (paused_since, last) {
                (None, Some((previous, _))) => {
                    let due = previous + HEARTBEAT - (qpc_ticks() - origin - paused);
                    Some(Duration::from_nanos(due.max(10_000) as u64 * 100))
                }
                _ => None,
            };
            let received = match next {
                Some(wait) => inbox.recv_timeout(wait),
                None => inbox
                    .recv()
                    .map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected),
            };
            let event = match received {
                Ok(event) => event,
                // A still screen delivers no frames. Repeat the last one
                // every second, so the video can be seeked and edited.
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if paused_since.is_none()
                        && let Some((previous, slot)) = last
                    {
                        let now = qpc_ticks() - origin - paused;
                        if now - previous >= HEARTBEAT {
                            if let Err(e) = self.writer.write(&self.nv12, slot, now, period) {
                                interrupted = Some(Interruption::from_error(&e));
                                log::error!("recording interrupted: {e:#}");
                                break;
                            }
                            last = Some((now, slot));
                            frames += 1;
                        }
                    }
                    continue;
                }
                Err(_) => Event::Stop,
            };
            match event {
                Event::Pause => {
                    paused_since.get_or_insert_with(qpc_ticks);
                }
                Event::Resume => {
                    if let Some(since) = paused_since.take() {
                        paused += qpc_ticks() - since;
                    }
                }
                Event::Stop => break,
                Event::DisplayGone => {
                    log::warn!("recording interrupted: the display went away");
                    interrupted = Some(Interruption::DisplayGone);
                    break;
                }
                Event::DisplayChanged => self.reread_white_scale(),
                Event::Frame => {
                    // Take every frame that is ready; the pool holds two.
                    let taken = (|| -> Result<()> {
                        while let Some(frame) = next_frame(&self.pool)? {
                            let captured = frame.SystemRelativeTime()?.Duration;
                            if paused_since.is_some() {
                                frame.Close()?;
                                continue;
                            }
                            let time = (captured - origin - paused).max(0);
                            if let Some((previous, _)) = last
                                && time < previous + period * 9 / 10
                            {
                                skipped_rate += 1;
                                frame.Close()?;
                                continue;
                            }
                            // Before the copy: a frame that cannot be encoded
                            // costs nothing.
                            let Some(slot) = self.nv12.free_slot() else {
                                dropped_busy += 1;
                                frame.Close()?;
                                continue;
                            };
                            let context = self.gpu.context.clone();
                            let measured = timer.as_mut().map(|t| t.begin(&context));
                            self.copy_region(&frame)?;
                            frame.Close()?;
                            if let (Some(t), Some(i)) = (&timer, measured) {
                                t.stamp(&context, i, 1);
                            }
                            let started = Instant::now();
                            self.convert(time)?;
                            let cpu_convert = started.elapsed();
                            if let (Some(t), Some(i)) = (&timer, measured) {
                                t.stamp(&context, i, 2);
                            }
                            self.nv12.convert(&self.gpu, slot)?;
                            if let (Some(t), Some(i)) = (&timer, measured) {
                                // Before the hand-off: the encoder works on the
                                // same queue from its own thread.
                                t.stamp(&context, i, 3);
                            }
                            let time = last.map_or(time, |(previous, _)| time.max(previous + 1));
                            let started = Instant::now();
                            self.writer.write(&self.nv12, slot, time, period)?;
                            if let (Some(t), Some(i)) = (&mut timer, measured) {
                                t.end(&context, i, cpu_convert, started.elapsed());
                            }
                            last = Some((time, slot));
                            frames += 1;
                        }
                        Ok(())
                    })();
                    if let Err(e) = taken {
                        interrupted = Some(Interruption::from_error(&e));
                        log::error!("recording interrupted: {e:#}");
                        break;
                    }
                }
            }
        }
        if let Some(since) = paused_since.take() {
            paused += qpc_ticks() - since;
        }
        let why = |e: anyhow::Error| match &interrupted {
            Some(i) => e.context(format!("after the recording was interrupted ({i:?})")),
            None => e,
        };
        // Repeat the last frame at the stop, so the video lasts until then.
        let end = qpc_ticks() - origin - paused;
        let paused = Duration::from_nanos(paused.max(0) as u64 * 100);
        let duration = match last {
            Some((time, slot)) => {
                let end = end.max(time + period);
                if end - period > time {
                    // Not after a failure: the encoder may be what failed.
                    if interrupted.is_none() {
                        self.writer
                            .write(&self.nv12, slot, end - period, period)
                            .map_err(why)?;
                        frames += 1;
                    }
                }
                let end = if interrupted.is_some() {
                    time + period
                } else {
                    end
                };
                Duration::from_nanos((end as u64) * 100)
            }
            None => return Err(why(anyhow::anyhow!("no frame was captured"))),
        };
        self.stop_capture();
        let timing = timer.map(|t| t.finish(&self.gpu.context));
        let hardware_encoder = self.writer.finish().map_err(why)?;
        Ok(RecordingSummary {
            timing,
            path: self.options.path.clone(),
            width: self.region.width,
            height: self.region.height,
            frames,
            dropped_busy,
            skipped_rate,
            duration,
            paused,
            hardware_encoder,
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
