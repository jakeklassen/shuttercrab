//! What the capture API deals in, on every OS: monitors, regions, frozen
//! frames and screenshots, its errors, and cutting a screenshot out of a
//! frozen screen. Nothing here touches the OS.

use crate::shape;
use std::{fmt, sync::Arc};

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

pub use shuttercrab_types::{MonitorId, WindowId};

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
    /// This OS has no backend for it yet.
    Unsupported,
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
    pub(crate) fn new(
        code: CaptureErrorCode,
        message: impl Into<String>,
        detail: impl fmt::Display,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            detail: Some(format!("{detail:#}")),
        }
    }

    /// What this OS has no backend for yet: `what` is not supported here.
    pub fn unsupported(what: &str) -> Self {
        Self {
            code: CaptureErrorCode::Unsupported,
            message: format!("{what} is not supported on this OS yet"),
            detail: None,
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

/// One monitor, captured and converted to SDR. Its pixels are the only copy:
/// the service keeps none, so whoever shows the frozen screen can take them
/// (see [`FrozenFrame::into_bgra`]) and cut the screenshot from them with
/// [`cut`] or [`cut_shape`].
pub struct FrozenFrame {
    pub(crate) monitor: MonitorInfo,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bgra: Vec<u8>,
    pub(crate) frame_peak: f32,
    pub(crate) hdr_regions: usize,
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
    pub fn bgra(&self) -> &[u8] {
        &self.bgra
    }

    /// The pixels themselves, without a copy.
    pub fn into_bgra(self) -> Vec<u8> {
        self.bgra
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
            .field("monitor", &self.monitor.device_name)
            .field("size", &(self.width, self.height))
            .finish()
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

/// Cut `region` (physical pixels) out of a frozen screen's pixels (`bgra`,
/// `width` × `height`, as [`FrozenFrame::bgra`] gives them) and encode it as
/// PNG. Takes some milliseconds: call it off the UI thread.
pub fn cut(bgra: &[u8], width: u32, height: u32, region: PhysicalRect) -> Result<Screenshot> {
    cut_masked(bgra, width, height, region, None)
}

/// Cut the inside of `outline` (physical pixels, closed automatically) out
/// of a frozen screen's pixels: its bounding box, with everything outside
/// the outline transparent. Takes some milliseconds: call it off the UI
/// thread.
pub fn cut_shape(
    bgra: &[u8],
    width: u32,
    height: u32,
    outline: &[(f32, f32)],
) -> Result<Screenshot> {
    let Some((x, y, w, h)) = shape::bounds(outline, width, height) else {
        return Err(CaptureError::new(
            CaptureErrorCode::InvalidRegion,
            "The outline is empty or outside the screen",
            format!("{} points", outline.len()),
        ));
    };
    let relative: Vec<_> = outline
        .iter()
        .map(|&(px, py)| (px - x as f32, py - y as f32))
        .collect();
    cut_masked(
        bgra,
        width,
        height,
        PhysicalRect::new(x, y, w, h),
        Some(&relative),
    )
}

/// Cut `region`; with `outline` (relative to the region), clear everything
/// outside it.
fn cut_masked(
    bgra: &[u8],
    width: u32,
    height: u32,
    region: PhysicalRect,
    outline: Option<&[(f32, f32)]>,
) -> Result<Screenshot> {
    let mut rgba = crop_bgra_to_rgba(bgra, width, height, region)?;
    if let Some(outline) = outline {
        shape::mask_outside(&mut rgba, region.width, region.height, outline);
    }
    let png = encode_png(region.width, region.height, &rgba)?;
    Ok(Screenshot {
        width: region.width,
        height: region.height,
        rgba: Arc::new(rgba),
        png: Arc::new(png),
    })
}

/// Premultiplied BGRA8 (what the compositor delivers) to straight RGBA8.
pub fn premultiplied_bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
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

/// `rgba` (straight-alpha RGBA8, `width` × `height`) as an sRGB PNG file, fast
/// rather than small.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
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

    #[test]
    fn unpremultiplies_window_pixels() {
        // BGRA: opaque, half-covered 100 (premultiplied to 50), transparent.
        let bgra = [10, 20, 30, 255, 50, 50, 50, 128, 7, 7, 7, 0];
        assert_eq!(
            premultiplied_bgra_to_rgba(&bgra),
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
