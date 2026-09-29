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
}

enum Event {
    Frame,
    Pause,
    Resume,
    Stop,
}

/// A recording in progress, on its own thread.
pub struct Recorder {
    events: Sender<Event>,
    thread: Option<JoinHandle<Result<RecordingSummary>>>,
}

impl Recorder {
    /// Start recording. Returns once the capture and the encoder are running.
    pub fn start(options: RecordOptions) -> Result<Self> {
        let (events, inbox) = channel();
        let (ready, started) = channel::<Result<()>>();
        let frames = events.clone();
        let thread = std::thread::Builder::new()
            .name("framecut-record".into())
            .spawn(move || record(options, frames, inbox, ready))
            .context("could not start the recording thread")?;
        match started.recv() {
            Ok(Ok(())) => Ok(Self {
                events,
                thread: Some(thread),
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
        let full = PhysicalRect::new(0, 0, monitor.bounds.width, monitor.bounds.height);
        let region = options
            .region
            .unwrap_or(full)
            .clamp_to(full.width, full.height);
        // NV12 needs even sizes.
        let region = PhysicalRect::new(region.x, region.y, region.width & !1, region.height & !1);
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
        let writer = Mp4Writer::new(&gpu, &options.path, w, h, options.fps)?;

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
        let frame_token = pool.FrameArrived(&TypedEventHandler::new(move |_, _| {
            let _ = frames.send(Event::Frame);
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
        loop {
            let event = match inbox.recv_timeout(Duration::from_millis(250)) {
                Ok(event) => event,
                // A still screen delivers no frames. Repeat the last one
                // every second, so the video can be seeked and edited.
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if paused_since.is_none()
                        && let Some((previous, slot)) = last
                    {
                        let now = qpc_ticks() - origin - paused;
                        if now - previous >= HEARTBEAT {
                            self.writer.write(&self.nv12, slot, now, period)?;
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
                Event::Frame => {
                    // Take every frame that is ready; the pool holds two.
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
                }
            }
        }
        if let Some(since) = paused_since.take() {
            paused += qpc_ticks() - since;
        }
        // Repeat the last frame at the stop, so the video lasts until then.
        let end = qpc_ticks() - origin - paused;
        let paused = Duration::from_nanos(paused.max(0) as u64 * 100);
        let duration = match last {
            Some((time, slot)) => {
                let end = end.max(time + period);
                if end - period > time {
                    self.writer.write(&self.nv12, slot, end - period, period)?;
                    frames += 1;
                }
                Duration::from_nanos((end as u64) * 100)
            }
            None => bail!("no frame was captured"),
        };
        self.stop_capture();
        let hardware_encoder = self.writer.finish()?;
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
        })
    }

    /// Stop capturing, once: removing the FrameArrived handler a second
    /// time with the same token makes Windows end the process.
    fn stop_capture(&mut self) {
        if let Some(token) = self.frame_token.take() {
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
    fn new(gpu: &Gpu, path: &Path, w: u32, h: u32, fps: u32) -> Result<Self> {
        unsafe {
            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.context("no DXGI device manager")?;
            manager.ResetDevice(&gpu.device, token)?;

            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 3)?;
            let attributes = attributes.context("no attributes")?;
            attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            attributes.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &manager)?;

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
