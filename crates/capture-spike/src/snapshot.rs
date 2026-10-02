//! Capture one monitor into a directory of artifacts:
//!
//! - `capture.png`: the product output. HDR and WCG monitors go through the
//!   FP16 path and the shader; SDR monitors use the plain 8-bit capture,
//!   which is the desktop exactly (PRD §9.5).
//! - `source.fp16`: the FP16 frame as the compositor delivered it.
//! - `windows-8bit.png`: Windows' own 8-bit capture of the same monitor. On
//!   SDR it is the ground truth; on HDR it shows the naive result.
//! - `report.json`: the monitor descriptor, the math applied, and timings.

use crate::{
    capture::{self, PixelFormat},
    color::{self, ColorMode, Highlights},
    display::{self, Monitor},
    gpu::{Gpu, SdrConverter},
    png_io,
    raw::RawFrame,
};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

pub struct Snapshot {
    pub dir: PathBuf,
    pub device_name: String,
    pub color_mode: ColorMode,
    pub sdr_white_level: Option<u32>,
    pub white_scale: f32,
    pub width: u32,
    pub height: u32,
    pub frame_peak: f32,
}

impl Snapshot {
    pub fn capture_png(&self) -> PathBuf {
        self.dir.join("capture.png")
    }

    pub fn source(&self) -> PathBuf {
        self.dir.join("source.fp16")
    }

    pub fn windows_png(&self) -> PathBuf {
        self.dir.join("windows-8bit.png")
    }
}

/// BGRA → RGBA, forcing alpha opaque.
fn bgra_to_rgba(mut bytes: Vec<u8>) -> Vec<u8> {
    for p in bytes.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
        p[3] = 255;
    }
    bytes
}

/// Capture `monitor` (see `display::select`) into `dir`, which must not exist yet.
pub fn take(monitor: Option<&str>, dir: &Path, highlights: Highlights) -> Result<Snapshot> {
    let monitors = display::enumerate()?;
    let before: &Monitor = display::select(&monitors, monitor)?;
    let white_scale = before.white_scale()?;
    let gpu = Gpu::hardware(Some(&before.adapter))?;
    let converter = SdrConverter::new(&gpu)?;

    let started = Instant::now();
    let fp16 = capture::capture_monitor(&gpu, before.hmonitor, PixelFormat::Fp16, false)?;
    let fp16_ms = started.elapsed().as_secs_f64() * 1000.0;
    let bgra = capture::capture_monitor(&gpu, before.hmonitor, PixelFormat::Bgra8, false)?;

    // The frame and the metadata used to convert it must describe the same
    // display state. A change (an HDR toggle, a brightness change, a mode
    // change) between the two reads spoils the capture.
    let after_all = display::enumerate()?;
    let after = display::find(&after_all, &before.device_name)?;
    ensure!(
        after.state() == before.state(),
        "the display state of {} changed during capture; try again",
        before.device_name
    );
    ensure!(
        (fp16.width, fp16.height) == (before.bounds.width, before.bounds.height),
        "captured {}x{} but the monitor is {}x{}",
        fp16.width,
        fp16.height,
        before.bounds.width,
        before.bounds.height
    );

    let started = Instant::now();
    let sdr = converter.convert(&gpu, &fp16.texture, white_scale, highlights)?;
    let convert_ms = started.elapsed().as_secs_f64() * 1000.0;
    let windows_rgba = bgra_to_rgba(gpu.read_back(&bgra.texture)?);

    std::fs::create_dir_all(dir.parent().context("output directory has no parent")?)?;
    std::fs::create_dir(dir).with_context(|| format!("creating {}", dir.display()))?;
    let snapshot = Snapshot {
        dir: dir.to_owned(),
        device_name: before.device_name.clone(),
        color_mode: before.color_mode,
        sdr_white_level: before.sdr_white_level,
        white_scale,
        width: fp16.width,
        height: fp16.height,
        frame_peak: sdr.frame_peak,
    };
    RawFrame {
        width: fp16.width,
        height: fp16.height,
        white_scale,
        color_mode: before.color_mode,
        data: gpu.read_back(&fp16.texture)?,
    }
    .write(&snapshot.source())?;
    png_io::write_srgb(
        &snapshot.windows_png(),
        bgra.width,
        bgra.height,
        &windows_rgba,
    )?;

    // On SDR the 8-bit capture is the product path. The FP16 path is still
    // measured against it: the two should agree exactly.
    let (product, path_name) = match before.color_mode {
        ColorMode::Sdr => (&windows_rgba, "8-bit capture (SDR path)"),
        ColorMode::Wcg | ColorMode::Hdr => (&sdr.rgba, "FP16 capture + shader"),
    };
    png_io::write_srgb(&snapshot.capture_png(), sdr.width, sdr.height, product)?;
    let fp16_vs_8bit = (sdr.rgba.len() == windows_rgba.len()).then(|| {
        let (mut max, mut differing) = (0u8, 0usize);
        for (a, b) in sdr.rgba.iter().zip(&windows_rgba) {
            let d = a.abs_diff(*b);
            max = max.max(d);
            differing += usize::from(d > 1);
        }
        json!({ "max_code_difference": max, "channels_differing_by_more_than_1": differing })
    });

    let report = json!({
        "tool": format!("capture-spike {}", env!("CARGO_PKG_VERSION")),
        "windows_build": windows_build(),
        "gpu": gpu.adapter_name,
        "monitor": before.to_json(),
        "product_path": path_name,
        "source_format": "R16G16B16A16_FLOAT, linear scRGB",
        "transform": {
            "version": color::TRANSFORM_VERSION,
            "white_scale": white_scale,
            "highlights": highlights.name(),
            "tone_knee": color::TONE_KNEE,
            "tile": color::TILE,
            "code_tolerance": color::CODE_TOLERANCE,
            "extended_epsilon": color::EXTENDED_EPSILON,
        },
        "frame": {
            "width": sdr.width,
            "height": sdr.height,
            "peak_relative_to_sdr_white": sdr.frame_peak,
            "hdr_content": sdr.frame_peak > 1.0 + color::EXTENDED_EPSILON,
        },
        "fp16_path_vs_windows_8bit": fp16_vs_8bit,
        "border_disabled": fp16.border_disabled,
        "timings_ms": { "fp16_capture": fp16_ms, "convert_and_read_back": convert_ms },
    });
    std::fs::write(
        snapshot.dir.join("report.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    Ok(snapshot)
}

pub use shuttercrab_capture::display::windows_build;

/// Local time as `YYYYMMDD-HHMMSS`, for directory names.
pub fn timestamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_becomes_opaque_rgba() {
        assert_eq!(
            bgra_to_rgba(vec![1, 2, 3, 0, 10, 20, 30, 40]),
            vec![3, 2, 1, 255, 30, 20, 10, 255]
        );
    }

    #[test]
    fn timestamps_sort_as_text() {
        let t = timestamp();
        assert_eq!(t.len(), 15);
        assert_eq!(&t[8..9], "-");
    }
}
