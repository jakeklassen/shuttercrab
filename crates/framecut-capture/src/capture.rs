//! One frame of a monitor or a window through Windows.Graphics.Capture.

use crate::gpu::Gpu;
use anyhow::{Context, Result, bail, ensure};
use std::time::{Duration, Instant};
use windows::{
    Graphics::{
        Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
        DirectX::DirectXPixelFormat,
    },
    Win32::{
        Foundation::HWND,
        Graphics::{Direct3D11::*, Dxgi::Common::*, Gdi::HMONITOR},
        System::WinRT::{
            Direct3D11::IDirect3DDxgiInterfaceAccess,
            Graphics::Capture::IGraphicsCaptureItemInterop,
        },
    },
    core::{Interface, factory},
};

/// The pixel format to ask the compositor for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// `R16G16B16A16_FLOAT`, linear scRGB: what the compositor holds on
    /// Advanced Color displays, before any 8-bit conversion.
    Fp16,
    /// `B8G8R8A8_UNORM`, sRGB-encoded: the desktop as 8-bit SDR. On an SDR
    /// display this is the desktop exactly; on an HDR display it is
    /// Windows' own conversion, which ignores the SDR white level.
    Bgra8,
}

impl PixelFormat {
    fn winrt(self) -> DirectXPixelFormat {
        match self {
            PixelFormat::Fp16 => DirectXPixelFormat::R16G16B16A16Float,
            PixelFormat::Bgra8 => DirectXPixelFormat::B8G8R8A8UIntNormalized,
        }
    }

    fn dxgi(self) -> DXGI_FORMAT {
        match self {
            PixelFormat::Fp16 => DXGI_FORMAT_R16G16B16A16_FLOAT,
            PixelFormat::Bgra8 => DXGI_FORMAT_B8G8R8A8_UNORM,
        }
    }
}

pub struct Frame {
    /// A copy the capture API no longer owns, bindable as a shader resource.
    pub texture: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    /// Whether Windows agreed to leave the yellow capture border off.
    pub border_disabled: bool,
}

/// Capture one frame of `monitor`. Blocks until the frame arrives (at most
/// five seconds). The pointer is drawn only with `include_cursor`.
pub fn capture_monitor(
    gpu: &Gpu,
    monitor: HMONITOR,
    format: PixelFormat,
    include_cursor: bool,
) -> Result<Frame> {
    ensure!(
        GraphicsCaptureSession::IsSupported()?,
        "Windows.Graphics.Capture is not available"
    );
    let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    let item: GraphicsCaptureItem =
        unsafe { interop.CreateForMonitor(monitor) }.context("CreateForMonitor failed")?;
    capture_item(gpu, &item, format, true, include_cursor)
}

/// Capture one frame of the top-level window `window`, as the window itself
/// draws it: parts covered by other windows are included. The frame has the
/// size of the window's visual, which may include transparent margins and
/// corners. Blocks until the frame arrives (at most five seconds).
pub fn capture_window(
    gpu: &Gpu,
    window: HWND,
    format: PixelFormat,
    include_cursor: bool,
) -> Result<Frame> {
    ensure!(
        GraphicsCaptureSession::IsSupported()?,
        "Windows.Graphics.Capture is not available"
    );
    let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    let item: GraphicsCaptureItem =
        unsafe { interop.CreateForWindow(window) }.context("CreateForWindow failed")?;
    capture_item(gpu, &item, format, false, include_cursor)
}

/// `exact`: fail if the content is not the item's size (a monitor that
/// changed mode). A window may be resizing; its frame is then cut to the
/// content that arrived.
fn capture_item(
    gpu: &Gpu,
    item: &GraphicsCaptureItem,
    format: PixelFormat,
    exact: bool,
    include_cursor: bool,
) -> Result<Frame> {
    let size = item.Size()?;
    ensure!(
        size.Width > 0 && size.Height > 0,
        "the capture target has no area"
    );
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &gpu.winrt_device()?,
        format.winrt(),
        1,
        size,
    )?;
    let session = match pool.CreateCaptureSession(item) {
        Ok(session) => session,
        Err(e) => {
            let _ = pool.Close();
            return Err(e).context("CreateCaptureSession failed");
        }
    };
    let result = (|| {
        session.SetIsCursorCaptureEnabled(include_cursor)?;
        // Cosmetic only; the border is not part of the captured image.
        let border_disabled = session.SetIsBorderRequired(false).is_ok();
        session.StartCapture().context("StartCapture failed")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let frame = loop {
            match pool.TryGetNextFrame() {
                Ok(frame) => break frame,
                // An empty pool returns a null frame, which windows-rs
                // surfaces as an error carrying a success code.
                Err(e) if e.code().is_ok() => {}
                Err(e) => return Err(e).context("TryGetNextFrame failed"),
            }
            if Instant::now() >= deadline {
                bail!("no frame arrived within five seconds");
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        let copy = (|| {
            let content = frame.ContentSize()?;
            if exact {
                ensure!(
                    content.Width == size.Width && content.Height == size.Height,
                    "the monitor changed size during capture; try again"
                );
            }
            let width = content.Width.min(size.Width) as u32;
            let height = content.Height.min(size.Height) as u32;
            ensure!(width > 0 && height > 0, "the frame is empty");
            let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
            let source: ID3D11Texture2D = unsafe { access.GetInterface()? };
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { source.GetDesc(&mut desc) };
            ensure!(
                desc.Format == format.dxgi(),
                "asked for {:?}, got {:?}",
                format.dxgi(),
                desc.Format
            );
            // The frame's texture may be larger than the content; keep only
            // the content.
            let texture =
                gpu.texture(width, height, desc.Format, D3D11_BIND_SHADER_RESOURCE, None)?;
            let region = D3D11_BOX {
                left: 0,
                top: 0,
                front: 0,
                right: width,
                bottom: height,
                back: 1,
            };
            unsafe {
                gpu.context
                    .CopySubresourceRegion(&texture, 0, 0, 0, 0, &source, 0, Some(&region))
            };
            Ok((texture, width, height))
        })();
        frame.Close()?;
        let (texture, width, height) = copy?;
        Ok(Frame {
            texture,
            width,
            height,
            border_disabled,
        })
    })();
    // Stop explicitly, on success and failure alike.
    let closed = session.Close().and(pool.Close());
    let frame = result?;
    closed?;
    Ok(frame)
}

/// Load and initialise Windows.Graphics.Capture without capturing anything:
/// the first use in a process costs a few hundred milliseconds.
pub fn warm_up(gpu: &Gpu) -> Result<()> {
    ensure!(
        GraphicsCaptureSession::IsSupported()?,
        "Windows.Graphics.Capture is not available"
    );
    let _interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    let size = windows::Graphics::SizeInt32 {
        Width: 1,
        Height: 1,
    };
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &gpu.winrt_device()?,
        PixelFormat::Fp16.winrt(),
        1,
        size,
    )?;
    pool.Close()?;
    Ok(())
}
