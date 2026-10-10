//! The capture service on Windows (PRD §19).
//!
//! All capture and GPU work runs on one service thread that owns the
//! Direct3D device, the compiled shaders and the WinRT apartment. Callers
//! hold a cheap, cloneable [`Capture`] and await its results; no COM or
//! Direct3D object crosses to them. A [`FrozenFrame`] is the converted image
//! of one monitor at one instant: the selection overlay shows it, and the
//! screenshot is cut from it, so what the user sees is what they get.

use super::{
    capture::{self, PixelFormat},
    display,
    gpu::{Gpu, SdrConverter},
};
use crate::{
    color::{ColorMode, Highlights, SCREENSHOT_ANCHOR},
    screen::{
        CaptureError, CaptureErrorCode, FrozenFrame, MonitorId, MonitorInfo, PhysicalRect, Result,
        Screenshot, WindowId, encode_png, premultiplied_bgra_to_rgba,
    },
};
use futures::channel::oneshot;
use std::{
    collections::HashMap,
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

enum Request {
    WarmUp,
    Monitors(oneshot::Sender<Result<Vec<MonitorInfo>>>),
    Freeze(MonitorId, bool, oneshot::Sender<Result<FrozenFrame>>),
    Window(WindowId, bool, oneshot::Sender<Result<Screenshot>>),
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
        std::thread::Builder::new()
            .name("shuttercrab-capture".into())
            .spawn(move || Service::new().run(inbox))
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

    /// Capture the top-level window `window` directly (PRD §7.3): its own
    /// content, including parts covered by other windows, converted to SDR
    /// for the monitor it is mostly on. Rounded corners stay transparent.
    pub fn capture_window(
        &self,
        window: WindowId,
        include_cursor: bool,
    ) -> impl Future<Output = Result<Screenshot>> + use<> {
        let (reply, answer) = oneshot::channel();
        self.ask(Request::Window(window, include_cursor, reply), answer)
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
    (!monitor.is_invalid()).then_some(monitor_id(monitor))
}

fn stopped() -> CaptureError {
    CaptureError {
        code: CaptureErrorCode::CaptureUnavailable,
        message: "The capture service stopped".into(),
        detail: None,
    }
}

struct Service {
    /// Device and shaders per adapter, created on first use.
    gpus: HashMap<String, (Gpu, SdrConverter)>,
}

impl Service {
    fn new() -> Self {
        Self {
            gpus: HashMap::new(),
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
                Request::Window(window, include_cursor, reply) => {
                    let captured = retry_once(
                        &mut self,
                        |s| s.capture_window(window, include_cursor),
                        Service::recover,
                    );
                    self.trim();
                    let _ = reply.send(captured);
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
        Ok(list_monitors()?.iter().map(describe).collect())
    }

    fn freeze(&mut self, id: MonitorId, include_cursor: bool) -> Result<FrozenFrame> {
        let started = Instant::now();
        let monitors = list_monitors()?;
        let monitor = monitors
            .iter()
            .find(|m| monitor_id(m.hmonitor) == id)
            .ok_or_else(|| {
                CaptureError::new(
                    CaptureErrorCode::MonitorGone,
                    "That monitor is no longer attached",
                    format!("{id:?}"),
                )
            })?;
        let white_scale = white_scale(monitor)?;
        let (gpu, converter) = self.gpu_for(monitor)?;
        let frame =
            capture_desktop(gpu, converter, monitor, white_scale, include_cursor).map_err(|e| {
                classify(
                    e,
                    CaptureErrorCode::CaptureUnavailable,
                    "Could not capture the screen",
                )
            })?;
        ensure_unchanged(monitor)?;
        log::debug!(
            "froze {} ({}x{}, {}) in {} ms",
            monitor.device_name,
            frame.width,
            frame.height,
            monitor.color_mode.name(),
            started.elapsed().as_millis()
        );
        Ok(frame)
    }

    /// Return pooled graphics memory after a capture.
    fn trim(&self) {
        for (gpu, _) in self.gpus.values() {
            gpu.trim();
        }
    }

    /// Get ready to try again after a failure `retry_once` retries.
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

    fn capture_window(&mut self, id: WindowId, include_cursor: bool) -> Result<Screenshot> {
        let started = Instant::now();
        let window = hwnd(id);
        let nearest = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
        let monitors = list_monitors()?;
        let monitor = monitors
            .iter()
            .find(|m| m.hmonitor == nearest)
            .ok_or_else(|| {
                CaptureError::new(
                    CaptureErrorCode::WindowGone,
                    "The window is not on a monitor",
                    format!("{:#x}", id.raw()),
                )
            })?;
        let white_scale = white_scale(monitor)?;
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
                (frame.width, frame.height, premultiplied_bgra_to_rgba(&bgra))
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

fn list_monitors() -> Result<Vec<display::Monitor>> {
    display::enumerate().map_err(|e| {
        CaptureError::new(
            CaptureErrorCode::CaptureUnavailable,
            "Could not list monitors",
            e,
        )
    })
}

fn white_scale(monitor: &display::Monitor) -> Result<f32> {
    monitor.white_scale().map_err(|e| {
        CaptureError::new(
            CaptureErrorCode::CaptureUnavailable,
            "Could not read the display's SDR white level",
            e,
        )
    })
}

/// One frame of `monitor`, as opaque 8-bit BGRA. SDR monitors give the
/// desktop as it is (PRD §9.5); Advanced Color monitors go through the FP16
/// path and the transform.
fn capture_desktop(
    gpu: &Gpu,
    converter: &SdrConverter,
    monitor: &display::Monitor,
    white_scale: f32,
    include_cursor: bool,
) -> anyhow::Result<FrozenFrame> {
    let capture = |format| capture::capture_monitor(gpu, monitor.hmonitor, format, include_cursor);
    let (width, height, bgra, frame_peak, hdr_regions) = match monitor.color_mode {
        ColorMode::Sdr => {
            let frame = capture(PixelFormat::Bgra8)?;
            let mut bgra = gpu.read_back(&frame.texture)?;
            for px in bgra.as_chunks_mut::<4>().0 {
                px[3] = 255;
            }
            (frame.width, frame.height, bgra, 1.0, 0)
        }
        ColorMode::Wcg | ColorMode::Hdr => {
            let frame = capture(PixelFormat::Fp16)?;
            let sdr = converter.convert_anchored(
                gpu,
                &frame.texture,
                white_scale,
                Highlights::Tonemap,
                SCREENSHOT_ANCHOR,
            )?;
            let mut bgra = sdr.rgba;
            for px in bgra.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
                px[3] = 255;
            }
            let regions = sdr.regions.len();
            (sdr.width, sdr.height, bgra, sdr.frame_peak, regions)
        }
    };
    Ok(FrozenFrame {
        monitor: describe(monitor),
        width,
        height,
        bgra,
        frame_peak,
        hdr_regions,
    })
}

/// Fail if `monitor`'s display state changed since it was looked up: the
/// frame and the metadata used to convert it must describe the same state.
fn ensure_unchanged(monitor: &display::Monitor) -> Result<()> {
    let after = display::enumerate().map_err(|e| {
        classify(
            e,
            CaptureErrorCode::CaptureUnavailable,
            "Could not capture the screen",
        )
    })?;
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
    Ok(())
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

/// Whether `e` means the Direct3D device is gone ([`DEVICE_LOST`]), as a
/// Windows error anywhere in its chain or as a code in its text.
pub fn is_device_lost(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause
            .downcast_ref::<windows::core::Error>()
            .is_some_and(|e| DEVICE_LOST.contains(&(e.code().0 as u32)))
    }) || {
        let text = format!("{e:#}").to_ascii_uppercase();
        DEVICE_LOST
            .iter()
            .any(|hr| text.contains(&format!("0X{hr:08X}")))
    }
}

/// A failure as the user should hear it: a lost device, or `code` with
/// `message`.
fn classify(e: anyhow::Error, code: CaptureErrorCode, message: &str) -> CaptureError {
    if is_device_lost(&e) {
        CaptureError::new(
            CaptureErrorCode::DeviceLost,
            "The graphics device was reset; try again",
            e,
        )
    } else {
        CaptureError::new(code, message, e)
    }
}

fn describe(monitor: &display::Monitor) -> MonitorInfo {
    let b = monitor.bounds;
    MonitorInfo {
        id: monitor_id(monitor.hmonitor),
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

/// The `HMONITOR` a [`MonitorId`] names.
pub fn hmonitor(id: MonitorId) -> HMONITOR {
    HMONITOR(id.raw() as _)
}

/// The [`MonitorId`] for an `HMONITOR`.
pub fn monitor_id(monitor: HMONITOR) -> MonitorId {
    MonitorId::from_raw(monitor.0 as u64)
}

/// The `HWND` a [`WindowId`] names.
pub fn hwnd(id: WindowId) -> HWND {
    HWND(id.raw() as _)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
