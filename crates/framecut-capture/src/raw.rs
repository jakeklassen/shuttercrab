//! A minimal container for captured FP16 frames, so the exact compositor
//! values can be inspected and re-converted later.
//!
//! Layout, all little-endian:
//!
//! ```text
//! 0   8 bytes  magic "FCFP16", 0, 1
//! 8   u32      width
//! 12  u32      height
//! 16  f32      white scale S used at capture time
//! 20  u32      color mode (0 SDR, 1 WCG, 2 HDR)
//! 24  width × height × RGBA IEEE binary16, rows top to bottom, no padding
//! ```

use crate::color::{self, ColorMode};
use anyhow::{Context, Result, bail, ensure};
use half::f16;
use std::path::Path;

const MAGIC: &[u8; 8] = &[b'F', b'C', b'F', b'P', b'1', b'6', 0, 1];
const HEADER: usize = 24;

pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub white_scale: f32,
    pub color_mode: ColorMode,
    /// Tightly packed little-endian RGBA binary16.
    pub data: Vec<u8>,
}

impl RawFrame {
    pub fn write(&self, path: &Path) -> Result<()> {
        let mut bytes = Vec::with_capacity(HEADER + self.data.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&self.width.to_le_bytes());
        bytes.extend_from_slice(&self.height.to_le_bytes());
        bytes.extend_from_slice(&self.white_scale.to_le_bytes());
        let mode: u32 = match self.color_mode {
            ColorMode::Sdr => 0,
            ColorMode::Wcg => 1,
            ColorMode::Hdr => 2,
        };
        bytes.extend_from_slice(&mode.to_le_bytes());
        bytes.extend_from_slice(&self.data);
        std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
    }

    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        ensure!(
            bytes.len() >= HEADER && &bytes[..8] == MAGIC,
            "{} is not a capture-spike FP16 frame",
            path.display()
        );
        let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
        let (width, height) = (u32_at(8), u32_at(12));
        let white_scale = f32::from_le_bytes(bytes[16..20].try_into().unwrap());
        let color_mode = match u32_at(20) {
            0 => ColorMode::Sdr,
            1 => ColorMode::Wcg,
            2 => ColorMode::Hdr,
            other => bail!("unknown color mode {other} in {}", path.display()),
        };
        ensure!(
            bytes.len() == HEADER + width as usize * height as usize * 8,
            "{} is truncated",
            path.display()
        );
        Ok(Self {
            width,
            height,
            white_scale,
            color_mode,
            data: bytes[HEADER..].to_vec(),
        })
    }

    pub fn pixel(&self, x: u32, y: u32) -> [f32; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 8;
        std::array::from_fn(|c| {
            f16::from_le_bytes([self.data[i + 2 * c], self.data[i + 2 * c + 1]]).to_f32()
        })
    }

    pub fn pixels(&self) -> Vec<[f32; 4]> {
        self.data
            .as_chunks::<8>()
            .0
            .iter()
            .map(|p| std::array::from_fn(|c| f16::from_le_bytes([p[2 * c], p[2 * c + 1]]).to_f32()))
            .collect()
    }

    /// The per-frame analysis the shader does on this frame.
    pub fn analysis(&self) -> color::FrameAnalysis {
        color::FrameAnalysis::new(&self.pixels(), self.width, self.height, self.white_scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_a_file() {
        let values = [0.0f32, 1.0, 2.5, -0.25, 65504.0, 0.000_1, 3.0, 1.0];
        let data = values
            .iter()
            .flat_map(|v| f16::from_f32(*v).to_le_bytes())
            .collect();
        let frame = RawFrame {
            width: 2,
            height: 1,
            white_scale: 2.5,
            color_mode: ColorMode::Hdr,
            data,
        };
        let path =
            std::env::temp_dir().join(format!("capture-spike-raw-{}.fp16", std::process::id()));
        frame.write(&path).unwrap();
        let back = RawFrame::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!((back.width, back.height, back.white_scale), (2, 1, 2.5));
        assert_eq!(back.color_mode, ColorMode::Hdr);
        assert_eq!(back.pixel(0, 0), [0.0, 1.0, 2.5, -0.25]);
        assert_eq!(back.pixel(1, 0)[0], 65504.0);
        assert_eq!(back.pixels().len(), 2);
    }
}
