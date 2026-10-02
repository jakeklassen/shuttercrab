//! The capture API the application uses (PRD §19).
//!
//! All capture and GPU work runs on one service thread that owns the
//! Direct3D device, the compiled shaders and the WinRT apartment. Callers
//! hold a cheap, cloneable [`Capture`] and await its results; no COM or
//! Direct3D object crosses to them. A [`FrozenFrame`] is the converted image
//! of one monitor at one instant: the selection overlay shows it, and the
//! screenshot is cut from it, so what the user sees is what they get.

use crate::{
    capture::{self, PixelFormat},
    color::{ColorMode, Highlights, SCREENSHOT_ANCHOR},
    display,
    gpu::{Gpu, SdrConverter},
};
use futures::channel::oneshot;
use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc,
        mpsc::{Sender, channel},
    },
    time::Instant,
};
use windows::Win32::{
    Foundation::{HWND, POINT},
    Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTONEAREST, MonitorFromPoint, MonitorFromWindow},
    System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize},
    UI::{
        HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
        WindowsAndMessaging::GetCursorPos,
    },
};

/// A rectangle in physical pixels. Monitor bounds are in virtual-desktop
/// coordinates; a screenshot region is relative to its monitor's top-left.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PhysicalRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl PhysicalRect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// The part of `self` inside `0..width` × `0..height`.
    pub fn clamp_to(&self, width: u32, height: u32) -> PhysicalRect {
        let x0 = self.x.clamp(0, width as i32);
        let y0 = self.y.clamp(0, height as i32);
        let x1 = (self.x + self.width as i32).clamp(0, width as i32);
        let y1 = (self.y + self.height as i32).clamp(0, height as i32);
        PhysicalRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
    }
}

/// A monitor, identified by its `HMONITOR` (which GPUI also uses as its
/// display id on Windows).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MonitorId(pub u64);

#[derive(Clone, Debug, PartialEq)]
pub struct MonitorInfo {
    pub id: MonitorId,
    /// GDI device name, e.g. `\\.\DISPLAY1`.
    pub device_name: String,
    pub name: String,
    /// Physical pixels, in virtual-desktop coordinates.
    pub bounds: PhysicalRect,
    pub scale_factor: f32,
    pub advanced_color_enabled: bool,
    pub hdr_enabled: bool,
    pub sdr_white_level_nits: Option<f32>,
    /// The graphics adapter driving the monitor.
    pub adapter: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureErrorCode {
    CaptureUnavailable,
    MonitorGone,
    DisplayChanged,
    DeviceLost,
    InvalidRegion,
    EncodeFailed,
    /// The window closed, was minimised, or cannot be captured.
    WindowGone,
}

/// A capture failure: a stable code, a message for the user, and detail for
/// the log (PRD §24).
#[derive(Clone, Debug)]
pub struct CaptureError {
    pub code: CaptureErrorCode,
    pub message: String,
    pub detail: Option<String>,
}

impl CaptureError {
    fn new(code: CaptureErrorCode, message: impl Into<String>, detail: impl fmt::Display) -> Self {
        Self {
            code,
            message: message.into(),
            detail: Some(format!("{detail:#}")),
        }
    }
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.detail {
            Some(detail) => write!(f, "{} ({detail})", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for CaptureError {}

pub type Result<T> = std::result::Result<T, CaptureError>;

/// One monitor, captured and converted to SDR, held until dropped.
pub struct FrozenFrame {
    id: u64,
    monitor: MonitorInfo,
    width: u32,
    height: u32,
    preview: Arc<Vec<u8>>,
    frame_peak: f32,
    hdr_regions: usize,
    release: Sender<Request>,
}

impl FrozenFrame {
    pub fn monitor(&self) -> &MonitorInfo {
        &self.monitor
    }

    /// Size in physical pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The converted image, tightly packed BGRA8 (the order GPUI's
    /// `RenderImage` takes), top row first. The screenshot is cut from
    /// exactly these pixels.
    pub fn preview_bgra(&self) -> &Arc<Vec<u8>> {
        &self.preview
    }

    /// Peak of the frame over SDR white; above 1 means HDR content.
    pub fn frame_peak(&self) -> f32 {
        self.frame_peak
    }

    /// How many HDR content regions were tone mapped.
    pub fn hdr_regions(&self) -> usize {
        self.hdr_regions
    }
}

impl fmt::Debug for FrozenFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrozenFrame")
            .field("id", &self.id)
            .field("monitor", &self.monitor.device_name)
            .field("size", &(self.width, self.height))
            .finish()
    }
}

impl Drop for FrozenFrame {
    fn drop(&mut self) {
        let _ = self.release.send(Request::Release(self.id));
    }
}

/// A finished screenshot. Its pixels and PNG are shared, not copied, with
/// whatever takes them: the clipboard, the file, the thumbnail.
#[derive(Clone)]
pub struct Screenshot {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8 sRGB, top row first, straight alpha. Opaque,
    /// except for window captures' rounded corners and borders.
    pub rgba: Arc<Vec<u8>>,
    /// The same image as an sRGB PNG file.
    pub png: Arc<Vec<u8>>,
}

impl fmt::Debug for Screenshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Screenshot")
            .field("size", &(self.width, self.height))
            .field("png_bytes", &self.png.len())
            .finish()
    }
}

enum Request {
    WarmUp,
    Monitors(oneshot::Sender<Result<Vec<MonitorInfo>>>),
    Freeze(MonitorId, bool, oneshot::Sender<Result<FrozenFrame>>),
    Screenshot(u64, PhysicalRect, oneshot::Sender<Result<Screenshot>>),
    Window(isize, bool, oneshot::Sender<Result<Screenshot>>),
    Release(u64),
}

/// A handle to the capture service thread. Cloning is cheap.
#[derive(Clone)]
pub struct Capture {
    requests: Sender<Request>,
}

impl Capture {
    /// Start the service thread.
    pub fn start() -> Result<Self> {
        let (requests, inbox) = channel::<Request>();
        let own = requests.clone();
        std::thread::Builder::new()
            .name("shuttercrab-capture".into())
            .spawn(move || Service::new(own).run(inbox))
            .map_err(|e| {
                CaptureError::new(
                    CaptureErrorCode::CaptureUnavailable,
                    "Could not start capture",
                    e,
                )
            })?;
        Ok(Self { requests })
    }

    fn ask<T>(
        &self,
        request: Request,
        answer: oneshot::Receiver<Result<T>>,
    ) -> impl Future<Output = Result<T>> + use<T> {
        let sent = self.requests.send(request).is_ok();
        async move {
            if !sent {
                return Err(stopped());
            }
            answer.await.unwrap_or_else(|_| Err(stopped()))
        }
    }

    /// Create the Direct3D devices and compile the shaders now, so the first
    /// freeze is as fast as the rest. Returns at once; the work happens on
    /// the service thread.
    pub fn warm_up(&self) {
        let _ = self.requests.send(Request::WarmUp);
    }

    pub fn list_monitors(&self) -> impl Future<Output = Result<Vec<MonitorInfo>>> + use<> {
        let (reply, answer) = oneshot::channel();
        self.ask(Request::Monitors(reply), answer)
    }

    /// Capture `monitor` now and convert it to SDR. The pointer is in the
    /// image only with `include_cursor` (PRD §15).
    pub fn freeze_monitor(
        &self,
        monitor: MonitorId,
        include_cursor: bool,
    ) -> impl Future<Output = Result<FrozenFrame>> + use<> {
        let (reply, answer) = oneshot::channel();
        self.ask(Request::Freeze(monitor, include_cursor, reply), answer)
    }

    /// Cut `region` (physical pixels relative to the frame's monitor) out of
    /// `frame` and encode it as PNG.
    pub fn screenshot(
        &self,
        frame: &FrozenFrame,
        region: PhysicalRect,
    ) -> impl Future<Output = Result<Screenshot>> + use<> {
        let (reply, answer) = oneshot::channel();
        self.ask(Request::Screenshot(frame.id, region, reply), answer)
    }

    /// Capture the top-level window `hwnd` directly (PRD §7.3): its own
    /// content, including parts covered by other windows, converted to SDR
    /// for the monitor it is mostly on. Rounded corners stay transparent.
    pub fn capture_window(
        &self,
        hwnd: isize,
        include_cursor: bool,
    ) -> impl Future<Output = Result<Screenshot>> + use<> {
        let (reply, answer) = oneshot::channel();
        self.ask(Request::Window(hwnd, include_cursor, reply), answer)
    }
}

/// The monitor under the pointer. Cheap; no service round trip.
pub fn monitor_under_pointer() -> Option<MonitorId> {
    // Physical coordinates, whatever the calling thread's DPI awareness.
    let previous =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let mut point = POINT::default();
    let found = unsafe { GetCursorPos(&mut point) };
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
    unsafe { SetThreadDpiAwarenessContext(previous) };
    found.ok()?;
    (!monitor.is_invalid()).then_some(MonitorId(monitor.0 as u64))
}

fn stopped() -> CaptureError {
    CaptureError {
        code: CaptureErrorCode::CaptureUnavailable,
        message: "The capture service stopped".into(),
        detail: None,
    }
}

struct StoredFrame {
    width: u32,
    height: u32,
    bgra: Arc<Vec<u8>>,
}

struct Service {
    requests: Sender<Request>,
    /// Device and shaders per adapter, created on first use.
    gpus: HashMap<String, (Gpu, SdrConverter)>,
    frames: HashMap<u64, StoredFrame>,
    next_id: u64,
}

impl Service {
    fn new(requests: Sender<Request>) -> Self {
        Self {
            requests,
            gpus: HashMap::new(),
            frames: HashMap::new(),
            next_id: 1,
        }
    }

    fn run(mut self, inbox: std::sync::mpsc::Receiver<Request>) {
        // Windows.Graphics.Capture needs a WinRT apartment on this thread, and
        // monitor bounds are only physical pixels for DPI-aware code: make
        // this thread aware whatever the host process declares.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        for request in inbox {
            match request {
                Request::WarmUp => {
                    self.warm_up();
                    self.trim();
                }
                Request::Monitors(reply) => {
                    let _ = reply.send(self.monitors());
                }
                Request::Freeze(monitor, include_cursor, reply) => {
                    let frozen = retry_once(
                        &mut self,
                        |s| s.freeze(monitor, include_cursor),
                        Service::recover,
                    );
                    self.trim();
                    let _ = reply.send(frozen);
                }
                Request::Screenshot(id, region, reply) => {
                    let _ = reply.send(self.screenshot(id, region));
                }
                Request::Window(hwnd, include_cursor, reply) => {
                    let captured = retry_once(
                        &mut self,
                        |s| s.capture_window(hwnd, include_cursor),
                        Service::recover,
                    );
                    self.trim();
                    let _ = reply.send(captured);
                }
                Request::Release(id) => {
                    self.frames.remove(&id);
                }
            }
        }
    }

    fn warm_up(&mut self) {
        let started = Instant::now();
        if let Ok(monitors) = display::enumerate() {
            for monitor in &monitors {
                log::info!(
                    "monitor {}: {}x{} at ({}, {}), {} DPI, {}, SDR white {}, adapter {}",
                    monitor.device_name,
                    monitor.bounds.width,
                    monitor.bounds.height,
                    monitor.bounds.x,
                    monitor.bounds.y,
                    monitor.dpi,
                    monitor.color_mode.name(),
                    monitor
                        .sdr_white_nits()
                        .map_or("n/a".into(), |n| format!("{n:.0} nits")),
                    monitor.adapter_name
                );
                match self.gpu_for(monitor) {
                    Ok((gpu, _)) => {
                        if let Err(e) = capture::warm_up(gpu) {
                            log::warn!("warm-up failed on {}: {e:#}", monitor.device_name);
                        }
                    }
                    Err(e) => log::warn!("warm-up failed on {}: {e}", monitor.device_name),
                }
            }
        }
        log::info!("warmed up in {} ms", started.elapsed().as_millis());
    }

    fn monitors(&self) -> Result<Vec<MonitorInfo>> {
        let monitors = display::enumerate().map_err(|e| {
            CaptureError::new(
                CaptureErrorCode::CaptureUnavailable,
                "Could not list monitors",
                e,
            )
        })?;
        Ok(monitors.iter().map(describe).collect())
    }

    fn freeze(&mut self, id: MonitorId, include_cursor: bool) -> Result<FrozenFrame> {
        let started = Instant::now();
        let monitors = display::enumerate().map_err(|e| {
            CaptureError::new(
                CaptureErrorCode::CaptureUnavailable,
                "Could not list monitors",
                e,
            )
        })?;
        let monitor = monitors
            .iter()
            .find(|m| m.hmonitor.0 as u64 == id.0)
            .ok_or_else(|| {
                CaptureError::new(
                    CaptureErrorCode::MonitorGone,
                    "That monitor is no longer attached",
                    format!("{id:?}"),
                )
            })?;
        let white_scale = monitor.white_scale().map_err(|e| {
            CaptureError::new(
                CaptureErrorCode::CaptureUnavailable,
                "Could not read the display's SDR white level",
                e,
            )
        })?;
        let (gpu, converter) = self.gpu_for(monitor)?;
        let unavailable = |e: anyhow::Error| {
            classify(
                e,
                CaptureErrorCode::CaptureUnavailable,
                "Could not capture the screen",
            )
        };

        // SDR monitors take the desktop as it is (PRD §9.5); Advanced Color
        // monitors go through the FP16 path and the transform.
        let (width, height, bgra, frame_peak, hdr_regions) = match monitor.color_mode {
            ColorMode::Sdr => {
                let frame = capture::capture_monitor(
                    gpu,
                    monitor.hmonitor,
                    PixelFormat::Bgra8,
                    include_cursor,
                )
                .map_err(unavailable)?;
                let mut bgra = gpu.read_back(&frame.texture).map_err(unavailable)?;
                for px in bgra.as_chunks_mut::<4>().0 {
                    px[3] = 255;
                }
                (frame.width, frame.height, bgra, 1.0, 0)
            }
            ColorMode::Wcg | ColorMode::Hdr => {
                let frame = capture::capture_monitor(
                    gpu,
                    monitor.hmonitor,
                    PixelFormat::Fp16,
                    include_cursor,
                )
                .map_err(unavailable)?;
                let sdr = converter
                    .convert_anchored(
                        gpu,
                        &frame.texture,
                        white_scale,
                        Highlights::Tonemap,
                        SCREENSHOT_ANCHOR,
                    )
                    .map_err(unavailable)?;
                let mut bgra = sdr.rgba;
                for px in bgra.as_chunks_mut::<4>().0 {
                    px.swap(0, 2);
                    px[3] = 255;
                }
                (
                    sdr.width,
                    sdr.height,
                    bgra,
                    sdr.frame_peak,
                    sdr.regions.len(),
                )
            }
        };

        // The frame and the metadata used to convert it must describe the
        // same display state.
        let after = display::enumerate().map_err(unavailable)?;
        let changed = display::find(&after, &monitor.device_name)
            .map(|now| now.state() != monitor.state())
            .unwrap_or(true);
        if changed {
            return Err(CaptureError::new(
                CaptureErrorCode::DisplayChanged,
                "The display changed during capture; try again",
                &monitor.device_name,
            ));
        }

        let id = self.next_id;
        self.next_id += 1;
        let preview = Arc::new(bgra);
        self.frames.insert(
            id,
            StoredFrame {
                width,
                height,
                bgra: preview.clone(),
            },
        );
        log::debug!(
            "froze {} ({}x{}, {}) in {} ms",
            monitor.device_name,
            width,
            height,
            monitor.color_mode.name(),
            started.elapsed().as_millis()
        );
        Ok(FrozenFrame {
            id,
            monitor: describe(monitor),
            width,
            height,
            preview,
            frame_peak,
            hdr_regions,
            release: self.requests.clone(),
        })
    }

    /// Get ready to try again after a failure `retry_once` retries.
    /// Return pooled graphics memory after a capture.
    fn trim(&self) {
        for (gpu, _) in self.gpus.values() {
            gpu.trim();
        }
    }

    fn recover(&mut self, code: CaptureErrorCode) {
        if code == CaptureErrorCode::DeviceLost {
            // A reset or removed device stays unusable; start afresh.
            log::warn!("the graphics device was lost; creating a new one");
            self.gpus.clear();
        } else {
            log::info!("the display changed during capture; capturing again");
        }
    }

    fn gpu_for(&mut self, monitor: &display::Monitor) -> Result<&(Gpu, SdrConverter)> {
        let key = monitor.adapter_name.clone();
        if !self.gpus.contains_key(&key) {
            let gpu = Gpu::hardware(Some(&monitor.adapter)).map_err(|e| {
                CaptureError::new(
                    CaptureErrorCode::CaptureUnavailable,
                    "Could not start Direct3D",
                    e,
                )
            })?;
            let converter = SdrConverter::new(&gpu).map_err(|e| {
                CaptureError::new(
                    CaptureErrorCode::CaptureUnavailable,
                    "Could not compile the color shader",
                    e,
                )
            })?;
            self.gpus.insert(key.clone(), (gpu, converter));
        }
        Ok(&self.gpus[&key])
    }

    fn screenshot(&self, id: u64, region: PhysicalRect) -> Result<Screenshot> {
        let frame = self.frames.get(&id).ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::InvalidRegion,
                "The frozen screen was already released",
                id,
            )
        })?;
        let rgba = crop_bgra_to_rgba(&frame.bgra, frame.width, frame.height, region)?;
        let png = encode_png(region.width, region.height, &rgba)?;
        Ok(Screenshot {
            width: region.width,
            height: region.height,
            rgba: Arc::new(rgba),
            png: Arc::new(png),
        })
    }

    fn capture_window(&mut self, hwnd: isize, include_cursor: bool) -> Result<Screenshot> {
        let started = Instant::now();
        let window = HWND(hwnd as _);
        let nearest = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
        let monitors = display::enumerate().map_err(|e| {
            CaptureError::new(
                CaptureErrorCode::CaptureUnavailable,
                "Could not list monitors",
                e,
            )
        })?;
        let monitor = monitors
            .iter()
            .find(|m| m.hmonitor == nearest)
            .ok_or_else(|| {
                CaptureError::new(
                    CaptureErrorCode::WindowGone,
                    "The window is not on a monitor",
                    format!("{hwnd:#x}"),
                )
            })?;
        let white_scale = monitor.white_scale().map_err(|e| {
            CaptureError::new(
                CaptureErrorCode::CaptureUnavailable,
                "Could not read the display's SDR white level",
                e,
            )
        })?;
        let (gpu, converter) = self.gpu_for(monitor)?;
        let failed = |e: anyhow::Error| {
            classify(
                e,
                CaptureErrorCode::WindowGone,
                "Could not capture the window",
            )
        };
        let (width, height, rgba) = match monitor.color_mode {
            ColorMode::Sdr => {
                let frame =
                    capture::capture_window(gpu, window, PixelFormat::Bgra8, include_cursor)
                        .map_err(failed)?;
                let bgra = gpu.read_back(&frame.texture).map_err(failed)?;
                (frame.width, frame.height, unpremultiply_bgra(&bgra))
            }
            ColorMode::Wcg | ColorMode::Hdr => {
                let frame = capture::capture_window(gpu, window, PixelFormat::Fp16, include_cursor)
                    .map_err(failed)?;
                let sdr = converter
                    .convert_anchored(
                        gpu,
                        &frame.texture,
                        white_scale,
                        Highlights::Tonemap,
                        SCREENSHOT_ANCHOR,
                    )
                    .map_err(failed)?;
                (sdr.width, sdr.height, sdr.rgba)
            }
        };
        let png = encode_png(width, height, &rgba)?;
        log::debug!(
            "captured a {width}x{height} window on {} ({}) in {} ms",
            monitor.device_name,
            monitor.color_mode.name(),
            started.elapsed().as_millis()
        );
        Ok(Screenshot {
            width,
            height,
            rgba: Arc::new(rgba),
            png: Arc::new(png),
        })
    }
}

/// Run `attempt`; after a lost device or a display change, `recover` and
/// run it once more (PRD §25). Other failures, and a second failure, are
/// returned as they are.
fn retry_once<S, T>(
    state: &mut S,
    mut attempt: impl FnMut(&mut S) -> Result<T>,
    mut recover: impl FnMut(&mut S, CaptureErrorCode),
) -> Result<T> {
    match attempt(state) {
        Err(e)
            if matches!(
                e.code,
                CaptureErrorCode::DeviceLost | CaptureErrorCode::DisplayChanged
            ) =>
        {
            log::debug!("retrying after: {e}");
            recover(state, e.code);
            attempt(state)
        }
        result => result,
    }
}

/// HRESULTs meaning the Direct3D device is gone: removed, hung, reset, or
/// an internal driver error. Sleep and wake or a driver update can cause
/// them; the device never works again.
/// DXGI_ERROR_DEVICE_REMOVED, _HUNG, _RESET and DXGI_ERROR_DRIVER_INTERNAL_ERROR.
pub(crate) const DEVICE_LOST: [u32; 4] = [0x887A_0005, 0x887A_0006, 0x887A_0007, 0x887A_0020];

/// A failure as the user should hear it: a lost device, or `code` with
/// `message`.
fn classify(e: anyhow::Error, code: CaptureErrorCode, message: &str) -> CaptureError {
    let lost = e.chain().any(|cause| {
        cause
            .downcast_ref::<windows::core::Error>()
            .is_some_and(|e| DEVICE_LOST.contains(&(e.code().0 as u32)))
    }) || {
        let text = format!("{e:#}").to_ascii_uppercase();
        DEVICE_LOST
            .iter()
            .any(|hr| text.contains(&format!("0X{hr:08X}")))
    };
    if lost {
        CaptureError::new(
            CaptureErrorCode::DeviceLost,
            "The graphics device was reset; try again",
            e,
        )
    } else {
        CaptureError::new(code, message, e)
    }
}

/// Premultiplied BGRA8 (what the compositor delivers) to straight RGBA8.
pub fn unpremultiply_bgra(bgra: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(bgra.len());
    for px in bgra.as_chunks::<4>().0 {
        let [b, g, r, a] = *px;
        let straight = |c: u8| match a {
            0 => 0,
            255 => c,
            _ => ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8,
        };
        rgba.extend_from_slice(&[straight(r), straight(g), straight(b), a]);
    }
    rgba
}

fn describe(monitor: &display::Monitor) -> MonitorInfo {
    let b = monitor.bounds;
    MonitorInfo {
        id: MonitorId(monitor.hmonitor.0 as u64),
        device_name: monitor.device_name.clone(),
        name: monitor.friendly_name.clone(),
        bounds: PhysicalRect::new(b.x, b.y, b.width, b.height),
        scale_factor: monitor.scale_factor(),
        advanced_color_enabled: monitor.advanced_color_enabled(),
        hdr_enabled: monitor.hdr_enabled(),
        sdr_white_level_nits: monitor.sdr_white_nits(),
        adapter: monitor.adapter_name.clone(),
    }
}

/// Cut `region` out of a BGRA8 image as RGBA8. The region must lie inside
/// the image and not be empty.
pub fn crop_bgra_to_rgba(
    bgra: &[u8],
    width: u32,
    height: u32,
    region: PhysicalRect,
) -> Result<Vec<u8>> {
    let inside = region.x >= 0
        && region.y >= 0
        && region.x as u32 + region.width <= width
        && region.y as u32 + region.height <= height;
    if region.is_empty() || !inside {
        return Err(CaptureError::new(
            CaptureErrorCode::InvalidRegion,
            "The selected region is empty or outside the screen",
            format!("{region:?} in {width}x{height}"),
        ));
    }
    let mut rgba = Vec::with_capacity((region.width * region.height * 4) as usize);
    for y in region.y as u32..region.y as u32 + region.height {
        let start = ((y * width + region.x as u32) * 4) as usize;
        for px in bgra[start..start + (region.width * 4) as usize]
            .as_chunks::<4>()
            .0
        {
            rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
    }
    Ok(rgba)
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let failed = |e: png::EncodingError| {
        CaptureError::new(
            CaptureErrorCode::EncodeFailed,
            "Could not encode the PNG",
            e,
        )
    };
    let mut png_bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut png_bytes, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(failed)?;
    writer.write_image_data(rgba).map_err(failed)?;
    writer.finish().map_err(failed)?;
    // The buffer grew by doubling and may outlive the screenshot (an
    // unsaved one stays with its thumbnail): give the slack back.
    png_bytes.shrink_to_fit();
    Ok(png_bytes)
}

// Keep the HMONITOR type reachable for callers that match GPUI display ids.
#[doc(hidden)]
pub fn hmonitor(id: MonitorId) -> HMONITOR {
    HMONITOR(id.0 as _)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4×2 BGRA image whose pixel (x, y) is (b = x, g = y, r = 10 + x, a = 0).
    fn image() -> Vec<u8> {
        (0..2u8)
            .flat_map(|y| (0..4u8).flat_map(move |x| [x, y, 10 + x, 0]))
            .collect()
    }

    #[test]
    fn crops_to_opaque_rgba() {
        let rgba = crop_bgra_to_rgba(&image(), 4, 2, PhysicalRect::new(1, 1, 2, 1)).unwrap();
        assert_eq!(rgba, [11, 1, 1, 255, 12, 1, 2, 255]);
        let all = crop_bgra_to_rgba(&image(), 4, 2, PhysicalRect::new(0, 0, 4, 2)).unwrap();
        assert_eq!(all.len(), 32);
    }

    #[test]
    fn rejects_empty_and_out_of_bounds_regions() {
        for region in [
            PhysicalRect::new(0, 0, 0, 1),
            PhysicalRect::new(3, 0, 2, 1),
            PhysicalRect::new(-1, 0, 2, 1),
            PhysicalRect::new(0, 1, 1, 2),
        ] {
            let err = crop_bgra_to_rgba(&image(), 4, 2, region).unwrap_err();
            assert_eq!(err.code, CaptureErrorCode::InvalidRegion, "{region:?}");
        }
    }

    #[test]
    fn clamps_regions_to_the_monitor() {
        assert_eq!(
            PhysicalRect::new(-5, -5, 10, 10).clamp_to(4, 2),
            PhysicalRect::new(0, 0, 4, 2)
        );
        assert_eq!(
            PhysicalRect::new(3, 1, 10, 10).clamp_to(4, 2),
            PhysicalRect::new(3, 1, 1, 1)
        );
        assert!(PhysicalRect::new(9, 9, 3, 3).clamp_to(4, 2).is_empty());
    }

    fn failure(code: CaptureErrorCode) -> CaptureError {
        CaptureError {
            code,
            message: String::new(),
            detail: None,
        }
    }

    #[test]
    fn a_lost_device_or_display_change_is_retried_once() {
        for code in [
            CaptureErrorCode::DeviceLost,
            CaptureErrorCode::DisplayChanged,
        ] {
            // (attempts, recoveries)
            let mut seen = (0, Vec::new());
            let result = retry_once(
                &mut seen,
                |s| {
                    s.0 += 1;
                    if s.0 == 1 {
                        Err(failure(code))
                    } else {
                        Ok(s.0)
                    }
                },
                |s, code| s.1.push(code),
            );
            assert_eq!(result.unwrap(), 2);
            assert_eq!(seen.1, [code]);
        }
    }

    #[test]
    fn other_failures_and_second_failures_are_returned() {
        let mut attempts = 0;
        let result: Result<()> = retry_once(
            &mut attempts,
            |n| {
                *n += 1;
                Err(failure(CaptureErrorCode::InvalidRegion))
            },
            |_, _| panic!("nothing to recover from"),
        );
        assert_eq!(result.unwrap_err().code, CaptureErrorCode::InvalidRegion);
        assert_eq!(attempts, 1);
        let mut attempts = 0;
        let result: Result<()> = retry_once(
            &mut attempts,
            |n| {
                *n += 1;
                Err(failure(CaptureErrorCode::DeviceLost))
            },
            |_, _| {},
        );
        assert_eq!(result.unwrap_err().code, CaptureErrorCode::DeviceLost);
        assert_eq!(attempts, 2);
    }

    #[test]
    fn device_removal_and_resets_are_recognised() {
        for hr in DEVICE_LOST {
            let e = anyhow::Error::from(windows::core::Error::from_hresult(
                windows::core::HRESULT(hr as i32),
            ))
            .context("CopySubresourceRegion failed");
            let error = classify(
                e,
                CaptureErrorCode::CaptureUnavailable,
                "Could not capture the screen",
            );
            assert_eq!(error.code, CaptureErrorCode::DeviceLost, "{hr:#x}");
        }
        let other = classify(
            anyhow::anyhow!("no frame arrived within five seconds"),
            CaptureErrorCode::CaptureUnavailable,
            "Could not capture the screen",
        );
        assert_eq!(other.code, CaptureErrorCode::CaptureUnavailable);
        assert_eq!(other.message, "Could not capture the screen");
    }

    #[test]
    fn unpremultiplies_window_pixels() {
        // BGRA: opaque, half-covered 100 (premultiplied to 50), transparent.
        let bgra = [10, 20, 30, 255, 50, 50, 50, 128, 7, 7, 7, 0];
        assert_eq!(
            unpremultiply_bgra(&bgra),
            [30, 20, 10, 255, 100, 100, 100, 128, 0, 0, 0, 0]
        );
    }

    #[test]
    fn png_round_trips() {
        let rgba = crop_bgra_to_rgba(&image(), 4, 2, PhysicalRect::new(0, 0, 4, 2)).unwrap();
        let png_bytes = encode_png(4, 2, &rgba).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
            .read_info()
            .unwrap();
        let mut decoded = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut decoded).unwrap();
        assert_eq!(decoded, rgba);
        assert_eq!(
            reader.info().srgb,
            Some(png::SrgbRenderingIntent::Perceptual)
        );
    }
}
