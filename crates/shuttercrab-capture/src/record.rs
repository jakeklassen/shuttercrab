//! Screen recording (PRD §13; Milestone 3 spike): one monitor, cropped to a
//! region on the GPU, converted to SDR frame by frame, turned into NV12 by
//! the Direct3D video processor (BT.709), and encoded to H.264 in an MP4 by
//! Media Foundation, hardware accelerated where available.
//!
//! Frames never leave the GPU. Encoder input textures come from a small
//! fixed pool; a frame that arrives while all of them are still with the
//! encoder is dropped and counted, so no queue can grow without bound.
//!
//! The output timeline is Windows' capture time: paused time is removed,
//! frames faster than the frame rate are skipped, and a still screen (which
//! delivers no frames) keeps its duration because the last frame is
//! repeated at the stop time.

use crate::{
    color::{HdrRegion, Highlights, SCREENSHOT_ANCHOR, anchor_regions},
    display,
    gpu::{Analysis, Gpu, SdrConverter},
    service::{MonitorId, PhysicalRect, hmonitor},
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    thread::JoinHandle,
    time::Duration,
};
use windows::{
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
        DirectX::DirectXPixelFormat,
        SizeInt32,
    },
    Win32::{
        Foundation::{E_NOTIMPL, HMODULE},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1},
            Direct3D11::*,
            Dxgi::{Common::*, IDXGIAdapter},
        },
        Media::MediaFoundation::*,
        System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
        System::WinRT::{
            Direct3D11::IDirect3DDxgiInterfaceAccess,
            Graphics::Capture::IGraphicsCaptureItemInterop, RO_INIT_MULTITHREADED, RoInitialize,
        },
        UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
    },
    core::{HSTRING, Interface, Ref, factory, implement},
};

/// What to record.
#[derive(Clone, Debug)]
pub struct RecordOptions {
    pub monitor: MonitorId,
    /// Physical pixels relative to the monitor; `None` records all of it.
    /// Rounded down to even sizes, as NV12 requires.
    pub region: Option<PhysicalRect>,
    /// 30 or 60 (PRD §13.1).
    pub fps: u32,
    pub include_cursor: bool,
    pub path: PathBuf,
}

/// What a finished recording contains.
#[derive(Clone, Debug)]
pub struct RecordingSummary {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Frames written, including the repeat of the last frame at the stop.
    pub frames: u64,
    /// Frames dropped because every encoder texture was busy.
    pub dropped_busy: u64,
    /// Frames skipped because they came faster than the frame rate.
    pub skipped_rate: u64,
    /// The output duration: capture time minus paused time.
    pub duration: Duration,
    /// Time spent paused.
    pub paused: Duration,
    /// Whether Media Foundation chose a hardware encoder.
    pub hardware_encoder: bool,
    /// Why the recording ended before it was stopped, if it did. What was
    /// recorded until then is still in the file.
    pub interrupted: Option<Interruption>,
}

/// Why a recording ended by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Interruption {
    /// The recorded display was disconnected or turned off.
    DisplayGone,
    /// The graphics driver was reset or the device removed.
    DeviceLost,
    /// The file could not be written because the disk is full.
    DiskFull,
    /// Anything else, as the error said.
    Failed(String),
}

/// ERROR_HANDLE_DISK_FULL and ERROR_DISK_FULL, as HRESULTs.
const DISK_FULL: [u32; 2] = [0x8007_0027, 0x8007_0070];

impl Interruption {
    /// What `e`, an error during recording, means for the user.
    pub fn from_error(e: &anyhow::Error) -> Self {
        let codes: Vec<u32> = e
            .chain()
            .filter_map(|cause| cause.downcast_ref::<windows::core::Error>())
            .map(|e| e.code().0 as u32)
            .collect();
        let text = format!("{e:#}").to_ascii_uppercase();
        let any = |list: &[u32]| {
            list.iter()
                .any(|hr| codes.contains(hr) || text.contains(&format!("0X{hr:08X}")))
        };
        if any(&crate::service::DEVICE_LOST) {
            Self::DeviceLost
        } else if any(&DISK_FULL) {
            Self::DiskFull
        } else {
            Self::Failed(format!("{e:#}"))
        }
    }

    /// Why recording stopped, to finish "Recording stopped: …".
    pub fn describe(&self) -> String {
        match self {
            Self::DisplayGone => "the display was disconnected or turned off".into(),
            Self::DeviceLost => "the graphics driver was reset".into(),
            Self::DiskFull => "the disk is full".into(),
            Self::Failed(_) => "something went wrong (the log has the details)".into(),
        }
    }
}

enum Event {
    Frame,
    Pause,
    Resume,
    Stop,
    /// The recorded display went away: Windows closed the capture, or the
    /// app heard that the display is no longer attached.
    DisplayGone,
    /// The displays changed (HDR switched on or off, say): read the
    /// recorded display's white level again.
    DisplayChanged,
}

/// A recording in progress, on its own thread.
pub struct Recorder {
    events: Sender<Event>,
    thread: Option<JoinHandle<Result<RecordingSummary>>>,
    /// Resolves when the thread ends, however it ends.
    ended: Option<futures::channel::oneshot::Receiver<()>>,
}

impl Recorder {
    /// Start recording. Returns once the capture and the encoder are running.
    pub fn start(options: RecordOptions) -> Result<Self> {
        let (events, inbox) = channel();
        let (ready, started) = channel::<Result<()>>();
        // Dropped when the thread ends, which resolves `ended`.
        let (ending, ended) = futures::channel::oneshot::channel::<()>();
        let frames = events.clone();
        let thread = std::thread::Builder::new()
            .name("shuttercrab-record".into())
            .spawn(move || {
                let _ending = ending;
                record(options, frames, inbox, ready)
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

    /// The recorded display is gone: end the recording, keeping what was
    /// recorded, as if Windows had closed the capture.
    pub fn display_gone(&self) {
        let _ = self.events.send(Event::DisplayGone);
    }

    /// The displays changed and the recorded one is still attached: carry
    /// on with its white level as it is now, so a recording stays exposed
    /// right when HDR is switched on or off.
    pub fn display_changed(&self) {
        let _ = self.events.send(Event::DisplayChanged);
    }

    /// A future that resolves when the recording ends, by [`Recorder::stop`]
    /// or by itself (the display went away, the device was lost, the disk
    /// filled up). [`Recorder::stop`] then returns at once with what was
    /// saved and why it ended. `None` after the first call.
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

/// The smallest recording, physical pixels a side. Media Foundation's
/// software H.264 encoder refuses 32×32 and takes 34×34; smaller than this
/// is no use as a video anyway.
pub const MIN_SIDE: u32 = 64;

/// Hardware encoders have minimum sizes of their own (NVENC's H.264 needs
/// at least 145×49, and fails only once frames arrive), so smaller
/// recordings use the software encoder, which is quick at such sizes.
const HARDWARE_MIN_SIDE: u32 = 256;

/// What of a `width`×`height` monitor is recorded for `region`: all of it
/// for `None`; otherwise the region on the monitor, grown about its centre
/// to [`MIN_SIDE`] if smaller, with even sizes, as NV12 requires.
pub fn recordable(region: Option<PhysicalRect>, width: u32, height: u32) -> PhysicalRect {
    let full = PhysicalRect::new(0, 0, width, height);
    let r = region.unwrap_or(full).clamp_to(width, height);
    let grow = |at: i32, side: u32, limit: u32| {
        if side >= MIN_SIDE || limit < MIN_SIDE {
            return (at, side.min(limit));
        }
        let start = at - (MIN_SIDE - side) as i32 / 2;
        (start.clamp(0, (limit - MIN_SIDE) as i32), MIN_SIDE)
    };
    let (x, w) = grow(r.x, r.width, width);
    let (y, h) = grow(r.y, r.height, height);
    PhysicalRect::new(x, y, w & !1, h & !1)
}

/// Whether `monitor` is still attached. Unplugging a display does not close
/// its capture: Windows sends black frames, then none, and a display
/// plugged back in is a new one. Its handle stops being valid the moment it
/// goes, before the black frames, so the app checks it when Windows says
/// the displays changed and tells the recorder with
/// [`Recorder::display_gone`].
pub fn attached(monitor: MonitorId) -> bool {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(hmonitor(monitor), &mut info) }.as_bool()
}

/// 100-nanosecond units, Media Foundation's and Windows.Graphics.Capture's.
const TICKS_PER_SECOND: i64 = 10_000_000;

/// How long a still screen goes without a repeated frame.
const HEARTBEAT: i64 = TICKS_PER_SECOND;

/// Now, in the clock Windows.Graphics.Capture stamps frames with:
/// QueryPerformanceCounter in 100-nanosecond units.
fn qpc_ticks() -> i64 {
    let (mut counter, mut frequency) = (0i64, 0i64);
    unsafe {
        let _ = QueryPerformanceCounter(&mut counter);
        let _ = QueryPerformanceFrequency(&mut frequency);
    }
    (counter as i128 * TICKS_PER_SECOND as i128 / frequency.max(1) as i128) as i64
}

/// Keeps HDR exposure steady across frames: the 90th-percentile anchor of
/// each frame, eased towards over half a second, so video neither pumps nor
/// flickers.
struct Exposure {
    value: Option<f32>,
    last: i64,
}

impl Exposure {
    const SETTLE_SECONDS: f32 = 0.5;

    fn apply(&mut self, analysis: &mut Analysis, time: i64) {
        if analysis.regions.is_empty() {
            return;
        }
        anchor_regions(
            &mut analysis.regions,
            &analysis.tiles,
            analysis.tiles_x,
            SCREENSHOT_ANCHOR,
        );
        let target = analysis
            .regions
            .iter()
            .map(|r| r.peak)
            .fold(1.0f32, f32::max);
        let value = match self.value {
            None => target,
            Some(value) => {
                let dt = (time - self.last).max(0) as f32 / TICKS_PER_SECOND as f32;
                let ease = 1.0 - (-dt / Self::SETTLE_SECONDS).exp();
                value + (target - value) * ease
            }
        };
        self.value = Some(value);
        self.last = time;
        set_peaks(&mut analysis.regions, value);
    }
}

fn set_peaks(regions: &mut [HdrRegion], peak: f32) {
    for region in regions {
        region.peak = peak.max(1.0);
    }
}

fn record(
    options: RecordOptions,
    frames: Sender<Event>,
    inbox: Receiver<Event>,
    ready: Sender<Result<()>>,
) -> Result<RecordingSummary> {
    let setup = (|| {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).context("MFStartup failed")?;
        }
        Session::new(&options, frames)
    })();
    let mut session = match setup {
        Ok(session) => {
            let _ = ready.send(Ok(()));
            session
        }
        Err(e) => {
            let _ = ready.send(Err(anyhow::anyhow!("{e:#}")));
            return Err(e);
        }
    };
    let result = session.run(&inbox);
    drop(session);
    unsafe {
        let _ = MFShutdown();
    }
    result
}

/// One recording's capture, GPU work and encoder.
struct Session {
    options: RecordOptions,
    gpu: Gpu,
    converter: SdrConverter,
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
    fn new(options: &RecordOptions, frames: Sender<Event>) -> Result<Self> {
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
        let converter = SdrConverter::new(&gpu)?;
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

    fn run(&mut self, inbox: &Receiver<Event>) -> Result<RecordingSummary> {
        let period = TICKS_PER_SECOND / self.options.fps as i64;
        // One clock throughout: frames carry their capture time in QPC
        // ticks (100 ns), and pauses and the stop are measured on it too.
        // The output timeline starts when recording starts.
        let origin = qpc_ticks();
        let mut paused_since: Option<i64> = None;
        let mut paused: i64 = 0;
        let mut last: Option<(i64, usize)> = None;
        let (mut frames, mut dropped_busy, mut skipped_rate) = (0u64, 0u64, 0u64);
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
                            self.copy_region(&frame)?;
                            frame.Close()?;
                            let Some(slot) = self.nv12.free_slot() else {
                                dropped_busy += 1;
                                continue;
                            };
                            self.convert(time)?;
                            self.nv12.convert(&self.gpu, slot)?;
                            let time = last.map_or(time, |(previous, _)| time.max(previous + 1));
                            self.writer.write(&self.nv12, slot, time, period)?;
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
        let hardware_encoder = self.writer.finish().map_err(why)?;
        Ok(RecordingSummary {
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
        self.converter.convert_into(
            &self.gpu,
            &self.crop,
            self.white_scale,
            Highlights::Tonemap,
            &self.sdr,
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

/// RGBA → NV12 (BT.709, studio range) with the Direct3D video processor,
/// into a small pool of textures the encoder reads.
struct Nv12 {
    video: ID3D11VideoContext1,
    processor: ID3D11VideoProcessor,
    input: ID3D11VideoProcessorInputView,
    slots: Vec<Slot>,
}

struct Slot {
    texture: ID3D11Texture2D,
    view: ID3D11VideoProcessorOutputView,
    /// With the encoder; cleared when Media Foundation releases the sample.
    busy: Arc<AtomicBool>,
    release: IMFAsyncCallback,
}

/// How many frames can be with the encoder at once.
const SLOTS: usize = 4;

impl Nv12 {
    fn new(gpu: &Gpu, rgba: &ID3D11Texture2D, w: u32, h: u32, fps: u32) -> Result<Self> {
        let device: ID3D11VideoDevice = gpu.device.cast().context("no video device")?;
        let video: ID3D11VideoContext1 = gpu.context.cast().context("no video context")?;
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: fps,
                Denominator: 1,
            },
            InputWidth: w,
            InputHeight: h,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: fps,
                Denominator: 1,
            },
            OutputWidth: w,
            OutputHeight: h,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        unsafe {
            let enumerator = device.CreateVideoProcessorEnumerator(&content)?;
            let support = enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_R8G8B8A8_UNORM)?;
            ensure!(
                support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 != 0,
                "the video processor cannot read RGBA"
            );
            let processor = device.CreateVideoProcessor(&enumerator, 0)?;
            let mut input = None;
            device.CreateVideoProcessorInputView(
                rgba,
                &enumerator,
                &D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    FourCC: 0,
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPIV {
                            MipSlice: 0,
                            ArraySlice: 0,
                        },
                    },
                },
                Some(&mut input),
            )?;
            let mut slots = Vec::with_capacity(SLOTS);
            for _ in 0..SLOTS {
                let texture = gpu.texture(
                    w,
                    h,
                    DXGI_FORMAT_NV12,
                    D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
                    None,
                )?;
                let mut view = None;
                device.CreateVideoProcessorOutputView(
                    &texture,
                    &enumerator,
                    &D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                            Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                        },
                    },
                    Some(&mut view),
                )?;
                let busy = Arc::new(AtomicBool::new(false));
                slots.push(Slot {
                    texture,
                    view: view.context("no output view")?,
                    release: Released(busy.clone()).into(),
                    busy,
                });
            }
            video.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            video.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );
            video.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            video.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            Ok(Self {
                video,
                processor,
                input: input.context("no input view")?,
                slots,
            })
        }
    }

    fn free_slot(&self) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| !s.busy.load(Ordering::Acquire))
    }

    fn convert(&self, _gpu: &Gpu, slot: usize) -> Result<()> {
        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: std::mem::ManuallyDrop::new(Some(self.input.clone())),
            ..Default::default()
        };
        let streams = [stream];
        let result = unsafe {
            self.video
                .VideoProcessorBlt(&self.processor, &self.slots[slot].view, 0, &streams)
        };
        // Release the reference the stream took.
        let [mut stream] = streams;
        unsafe { std::mem::ManuallyDrop::drop(&mut stream.pInputSurface) };
        result.context("VideoProcessorBlt failed")
    }
}

/// Clears a slot's busy flag when Media Foundation releases its sample.
#[implement(IMFAsyncCallback)]
struct Released(Arc<AtomicBool>);

impl IMFAsyncCallback_Impl for Released_Impl {
    fn GetParameters(&self, _: *mut u32, _: *mut u32) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Invoke(&self, _: Ref<IMFAsyncResult>) -> windows::core::Result<()> {
        self.0.store(false, Ordering::Release);
        Ok(())
    }
}

/// An H.264 MP4 file written by Media Foundation's sink writer.
struct Mp4Writer {
    writer: IMFSinkWriter,
    stream: u32,
    _manager: IMFDXGIDeviceManager,
    finished: bool,
}

impl Mp4Writer {
    /// With `hardware`, Media Foundation may choose a hardware encoder.
    fn new(gpu: &Gpu, path: &Path, w: u32, h: u32, fps: u32, hardware: bool) -> Result<Self> {
        unsafe {
            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.context("no DXGI device manager")?;
            manager.ResetDevice(&gpu.device, token)?;

            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 4)?;
            let attributes = attributes.context("no attributes")?;
            attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, hardware as u32)?;
            attributes.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &manager)?;
            // MP4 whatever the file is called: the app writes to a
            // `.partial` name and renames the file once it is finished.
            attributes.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;

            let _ = std::fs::remove_file(path);
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path), None, &attributes)
                .with_context(|| format!("cannot write {}", path.display()))?;

            // PRD §13.1: H.264, Rec.709. About 0.1 bit per pixel per frame,
            // within 2–80 Mbit/s.
            let bitrate = ((w as u64 * h as u64 * fps as u64) / 10).clamp(2_000_000, 80_000_000);
            let output = video_type(&MFVideoFormat_H264, w, h, fps)?;
            output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate as u32)?;
            output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            let stream = writer.AddStream(&output)?;
            let input = video_type(&MFVideoFormat_NV12, w, h, fps)?;
            writer
                .SetInputMediaType(stream, &input, None)
                .context("the encoder does not take NV12 at this size")?;
            writer.BeginWriting().context("BeginWriting failed")?;
            Ok(Self {
                writer,
                stream,
                _manager: manager,
                finished: false,
            })
        }
    }

    /// Hand the slot's texture to the encoder at `time`.
    fn write(&self, nv12: &Nv12, slot: usize, time: i64, duration: i64) -> Result<()> {
        let slot = &nv12.slots[slot];
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &slot.texture, 0, false)?;
            let length = buffer.cast::<IMF2DBuffer>()?.GetContiguousLength()?;
            buffer.SetCurrentLength(length)?;
            let sample = MFCreateTrackedSample()?;
            sample.SetAllocator(&slot.release, None)?;
            let sample: IMFSample = sample.cast()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(duration)?;
            slot.busy.store(true, Ordering::Release);
            if let Err(e) = self.writer.WriteSample(self.stream, &sample) {
                slot.busy.store(false, Ordering::Release);
                return Err(e).context("WriteSample failed");
            }
        }
        Ok(())
    }

    /// Finish the file. Returns whether a hardware encoder was used.
    fn finish(&mut self) -> Result<bool> {
        self.finished = true;
        let hardware = self.hardware_encoder();
        unsafe { self.writer.Finalize() }.context("could not finish the video file")?;
        Ok(hardware)
    }

    /// A hardware encoder carries `MFT_ENUM_HARDWARE_URL_Attribute`.
    fn hardware_encoder(&self) -> bool {
        unsafe {
            let mut raw = std::ptr::null_mut();
            if self
                .writer
                .GetServiceForStream(
                    self.stream,
                    &windows::core::GUID::zeroed(),
                    &IMFTransform::IID,
                    &mut raw,
                )
                .is_err()
                || raw.is_null()
            {
                return false;
            }
            let transform = IMFTransform::from_raw(raw);
            transform
                .GetAttributes()
                .and_then(|attributes| attributes.GetStringLength(&MFT_ENUM_HARDWARE_URL_Attribute))
                .is_ok()
        }
    }
}

fn video_type(subtype: &windows::core::GUID, w: u32, h: u32, fps: u32) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64)?;
        t.SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1)?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        // Rec.709, studio range: what the video processor writes and what
        // players expect of SDR H.264.
        t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        Ok(t)
    }
}

impl Drop for Mp4Writer {
    fn drop(&mut self) {
        if !self.finished {
            let _ = unsafe { self.writer.Finalize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attached_displays_are_present_and_a_stale_handle_is_not() {
        let monitors = display::enumerate().unwrap();
        assert!(!monitors.is_empty());
        for m in &monitors {
            let id = MonitorId(m.hmonitor.0 as u64);
            assert!(attached(id), "{} is attached", m.device_name);
        }
        // A handle that names no display, as one unplugged does.
        assert!(!attached(MonitorId(0x7FFF_0001)));
    }

    #[test]
    fn interruptions_are_told_apart_by_their_error_codes() {
        use windows::core::{Error, HRESULT};
        let windows_error = |code: u32| {
            anyhow::Error::from(Error::from_hresult(HRESULT(code as i32)))
                .context("WriteSample failed")
        };
        assert_eq!(
            Interruption::from_error(&windows_error(0x887A_0005)),
            Interruption::DeviceLost
        );
        assert_eq!(
            Interruption::from_error(&windows_error(0x8007_0070)),
            Interruption::DiskFull
        );
        // Codes only in the text count too.
        let text = anyhow::anyhow!("copy failed: 0x887A0007");
        assert_eq!(Interruption::from_error(&text), Interruption::DeviceLost);
        let other = anyhow::anyhow!("the monitor changed size during the recording");
        assert!(matches!(
            Interruption::from_error(&other),
            Interruption::Failed(m) if m.contains("changed size")
        ));
        assert_eq!(
            Interruption::DisplayGone.describe(),
            "the display was disconnected or turned off"
        );
    }

    #[test]
    fn recorded_regions_are_even_on_screen_and_not_too_small() {
        let rect = |x, y, w, h| Some(PhysicalRect::new(x, y, w, h));
        // All of it, or the region with odd sizes made even.
        assert_eq!(
            recordable(None, 3840, 2160),
            PhysicalRect::new(0, 0, 3840, 2160)
        );
        assert_eq!(
            recordable(rect(10, 20, 1001, 777), 3840, 2160),
            PhysicalRect::new(10, 20, 1000, 776)
        );
        // Off the edge: cut to the monitor (40×60 here), then grown back
        // to the minimum inside it.
        assert_eq!(
            recordable(rect(3800, 2100, 400, 400), 3840, 2160),
            PhysicalRect::new(3776, 2096, 64, 64)
        );
        // Tiny: grown about its centre, kept on the monitor.
        assert_eq!(
            recordable(rect(100, 300, 20, 200), 3840, 2160),
            PhysicalRect::new(78, 300, 64, 200)
        );
        assert_eq!(
            recordable(rect(0, 0, 10, 10), 3840, 2160),
            PhysicalRect::new(0, 0, 64, 64)
        );
    }

    fn analysis(peaks: &[f32]) -> Analysis {
        use crate::color::{TILE, TileStats, find_regions};
        let tiles: Vec<TileStats> = peaks
            .iter()
            .enumerate()
            .map(|(i, &peak)| TileStats {
                peak,
                non_sdr: TILE * TILE,
                extended: 1,
                min_x: i as u32 * TILE,
                min_y: 0,
                max_x: i as u32 * TILE + TILE - 1,
                max_y: TILE - 1,
            })
            .collect();
        let regions = find_regions(&tiles, peaks.len() as u32, 1);
        Analysis {
            tiles,
            tiles_x: peaks.len() as u32,
            regions,
            frame_peak: peaks.iter().copied().fold(0.0, f32::max),
        }
    }

    #[test]
    fn exposure_eases_towards_a_new_scene_instead_of_jumping() {
        let mut exposure = Exposure {
            value: None,
            last: 0,
        };
        let mut first = analysis(&[4.0; 10]);
        exposure.apply(&mut first, 0);
        assert_eq!(first.regions[0].peak, 4.0);
        // The scene gets much brighter one frame later: the anchor moves only
        // part of the way (1/30 s of a 0.5 s settle).
        let mut next = analysis(&[8.0; 10]);
        exposure.apply(&mut next, TICKS_PER_SECOND / 30);
        let peak = next.regions[0].peak;
        assert!(peak > 4.0 && peak < 4.5, "{peak}");
        // After two seconds it has arrived.
        let mut later = analysis(&[8.0; 10]);
        exposure.apply(&mut later, 2 * TICKS_PER_SECOND);
        assert!((later.regions[0].peak - 8.0).abs() < 0.1);
    }

    #[test]
    fn frames_without_hdr_content_are_left_alone() {
        let mut exposure = Exposure {
            value: None,
            last: 0,
        };
        let mut plain = Analysis {
            tiles: Vec::new(),
            tiles_x: 0,
            regions: Vec::new(),
            frame_peak: 1.0,
        };
        exposure.apply(&mut plain, 0);
        assert!(plain.regions.is_empty());
        assert_eq!(exposure.value, None);
    }
}
