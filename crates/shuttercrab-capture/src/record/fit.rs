//! A recorded window's picture, fitted into the recording's frame: the
//! frame keeps the size the window had when recording started, and a window
//! resized since is scaled to fit inside it, keeping its shape, with black
//! bars where it does not fill it (`shaders/fit.hlsl`).

use crate::gpu::{Gpu, compile_source, default_buffer, srv, uav};
use anyhow::{Context, Result};
use windows::{
    Win32::Graphics::{Direct3D11::*, Dxgi::Common::DXGI_FORMAT_R16G16B16A16_FLOAT},
    core::s,
};

const SHADER_SOURCE: &str = include_str!("../../shaders/fit.hlsl");

/// The most samples per side a frame pixel averages: enough for a window
/// scaled to a quarter of its size.
const MAX_SAMPLES: u32 = 4;

/// Where a `content`-sized picture goes in a `frame`-sized frame: as large
/// as fits, keeping its shape, centred, on whole pixels. Returns its top
/// left and its size.
pub(super) fn placement(content: (u32, u32), frame: (u32, u32)) -> ((u32, u32), (u32, u32)) {
    let (cw, ch) = (content.0.max(1) as f64, content.1.max(1) as f64);
    let (fw, fh) = (frame.0 as f64, frame.1 as f64);
    let scale = (fw / cw).min(fh / ch);
    let w = ((cw * scale).round() as u32).clamp(1, frame.0);
    let h = ((ch * scale).round() as u32).clamp(1, frame.1);
    (((frame.0 - w) / 2, (frame.1 - h) / 2), (w, h))
}

/// Mirrors `cbuffer Fit` in the shader.
#[repr(C)]
struct Params {
    output_size: [u32; 2],
    offset: [f32; 2],
    fitted: [f32; 2],
    content_size: [f32; 2],
    source_size: [f32; 2],
    samples: u32,
    unused: u32,
}

/// Fits pictures into one output texture (FP16, with unordered access).
pub(super) struct Fitter {
    shader: ID3D11ComputeShader,
    sampler: ID3D11SamplerState,
    params: ID3D11Buffer,
    output: ID3D11UnorderedAccessView,
    size: (u32, u32),
    /// The picture copied out of the captured frame, which cannot be read
    /// by a shader; grown as needed.
    copy: Option<(ID3D11Texture2D, ID3D11ShaderResourceView, (u32, u32))>,
}

impl Fitter {
    pub(super) fn new(gpu: &Gpu, output: &ID3D11Texture2D) -> Result<Self> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { output.GetDesc(&mut desc) };
        let mut sampler = None;
        unsafe {
            gpu.device.CreateSamplerState(
                &D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                    AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                    MaxLOD: f32::MAX,
                    ..Default::default()
                },
                Some(&mut sampler),
            )?;
        }
        Ok(Self {
            shader: compile_source(gpu, SHADER_SOURCE, s!("fit.hlsl"), s!("fit"))?,
            sampler: sampler.context("CreateSamplerState returned no sampler")?,
            params: default_buffer(gpu, size_of::<Params>() as u32)?,
            output: uav(gpu, output)?,
            size: (desc.Width, desc.Height),
            copy: None,
        })
    }

    /// Fit the `content`-sized picture at the top left of `source` (FP16)
    /// into the output.
    pub(super) fn fit(
        &mut self,
        gpu: &Gpu,
        source: &ID3D11Texture2D,
        content: (u32, u32),
    ) -> Result<()> {
        let ctx = &gpu.context;
        let view = self.copy_of(gpu, source, content)?;
        let ((x, y), (w, h)) = placement(content, self.size);
        let shrink = (content.0 as f32 / w as f32).max(content.1 as f32 / h as f32);
        let source_size = self.copy.as_ref().map_or(content, |(_, _, size)| *size);
        let params = Params {
            output_size: [self.size.0, self.size.1],
            offset: [x as f32, y as f32],
            fitted: [w as f32, h as f32],
            content_size: [content.0 as f32, content.1 as f32],
            source_size: [source_size.0 as f32, source_size.1 as f32],
            samples: (shrink.ceil() as u32).clamp(1, MAX_SAMPLES),
            unused: 0,
        };
        unsafe {
            ctx.UpdateSubresource(&self.params, 0, None, (&raw const params).cast(), 0, 0);
            ctx.CSSetShader(&self.shader, None);
            ctx.CSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
            ctx.CSSetShaderResources(0, Some(&[Some(view)]));
            ctx.CSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.CSSetUnorderedAccessViews(0, 1, Some(&Some(self.output.clone())), None);
            ctx.Dispatch(self.size.0.div_ceil(8), self.size.1.div_ceil(8), 1);
            ctx.CSSetUnorderedAccessViews(0, 1, Some(&None), None);
            ctx.CSSetShaderResources(0, Some(&[None]));
            ctx.CSSetShader(None, None);
        }
        Ok(())
    }

    /// Copy the picture into a texture a shader can read, and return its
    /// view.
    fn copy_of(
        &mut self,
        gpu: &Gpu,
        source: &ID3D11Texture2D,
        content: (u32, u32),
    ) -> Result<ID3D11ShaderResourceView> {
        let fits = self
            .copy
            .as_ref()
            .is_some_and(|(_, _, (w, h))| content.0 <= *w && content.1 <= *h);
        if !fits {
            let texture = gpu.texture(
                content.0,
                content.1,
                DXGI_FORMAT_R16G16B16A16_FLOAT,
                D3D11_BIND_SHADER_RESOURCE,
                None,
            )?;
            let view = srv(gpu, &texture)?;
            self.copy = Some((texture, view, content));
        }
        let (texture, view, _) = self.copy.as_ref().expect("made above");
        let area = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: content.0,
            bottom: content.1,
            back: 1,
        };
        unsafe {
            gpu.context
                .CopySubresourceRegion(texture, 0, 0, 0, 0, source, 0, Some(&area))
        };
        Ok(view.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_size_fills_the_frame() {
        assert_eq!(placement((1280, 720), (1280, 720)), ((0, 0), (1280, 720)));
    }

    #[test]
    fn a_wider_window_gets_bars_above_and_below() {
        // Twice as wide, same height: half the size, centred vertically.
        assert_eq!(placement((2560, 720), (1280, 720)), ((0, 180), (1280, 360)));
    }

    #[test]
    fn a_taller_window_gets_bars_at_the_sides() {
        assert_eq!(placement((640, 1440), (1280, 720)), ((480, 0), (320, 720)));
    }

    #[test]
    fn a_smaller_window_is_scaled_up_to_fit() {
        assert_eq!(placement((640, 360), (1280, 720)), ((0, 0), (1280, 720)));
    }

    /// An FP16 texture of `w`×`h` from `pixel(x, y)`.
    fn picture(gpu: &Gpu, w: u32, h: u32, pixel: impl Fn(u32, u32) -> [f32; 4]) -> ID3D11Texture2D {
        let bytes: Vec<u8> = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .flat_map(|(x, y)| pixel(x, y))
            .flat_map(|c| half::f16::from_f32(c).to_le_bytes())
            .collect();
        gpu.texture(
            w,
            h,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_UNORDERED_ACCESS,
            Some((&bytes, w * 8)),
        )
        .unwrap()
    }

    /// Fit `source`'s top-left `content` into a fresh `frame`-sized output
    /// and read it back as RGBA floats, row by row.
    fn fitted(
        gpu: &Gpu,
        source: &ID3D11Texture2D,
        content: (u32, u32),
        frame: (u32, u32),
    ) -> Vec<[f32; 4]> {
        let output = picture(gpu, frame.0, frame.1, |_, _| [0.5; 4]);
        Fitter::new(gpu, &output)
            .unwrap()
            .fit(gpu, source, content)
            .unwrap();
        let bytes = gpu.read_back(&output).unwrap();
        bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|p| {
                let c = |i: usize| half::f16::from_le_bytes([p[i], p[i + 1]]).to_f32();
                [c(0), c(2), c(4), c(6)]
            })
            .collect()
    }

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
    const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.01)
    }

    #[test]
    fn the_gpu_fits_a_wider_window_between_black_bars() {
        let gpu = Gpu::warp().unwrap();
        // Red on the left, blue on the right, twice as wide as the frame
        // is, inside a texture larger than the picture (as after a resize).
        let source = picture(&gpu, 160, 80, |x, y| match (x, y) {
            (_, 32..) | (128.., _) => [9.0, 9.0, 9.0, 1.0],
            (..64, _) => RED,
            _ => BLUE,
        });
        let out = fitted(&gpu, &source, (128, 32), (64, 64));
        let at = |x: usize, y: usize| out[y * 64 + x];
        // Half the size: rows 24 to 40 hold the picture, the rest are bars.
        for y in [0, 10, 23, 40, 63] {
            assert!(close(at(32, y), BLACK), "bar at row {y}: {:?}", at(32, y));
        }
        for y in [24, 32, 39] {
            assert!(close(at(5, y), RED), "left at row {y}: {:?}", at(5, y));
            assert!(close(at(58, y), BLUE), "right at row {y}: {:?}", at(58, y));
        }
        // Nothing from outside the picture leaks in at its edges.
        assert!(out.iter().all(|p| p.iter().all(|&c| c <= 1.0)));
    }

    #[test]
    fn the_gpu_copies_a_same_size_window_unchanged() {
        let gpu = Gpu::warp().unwrap();
        let source = picture(&gpu, 32, 16, |x, _| if x % 2 == 0 { RED } else { BLUE });
        let out = fitted(&gpu, &source, (32, 16), (32, 16));
        for (i, p) in out.iter().enumerate() {
            let want = if i % 2 == 0 { RED } else { BLUE };
            assert!(close(*p, want), "pixel {i}: {p:?}");
        }
    }

    #[test]
    fn a_tiny_window_still_has_a_picture() {
        let ((x, y), (w, h)) = placement((1, 400), (1280, 720));
        assert!(w >= 1 && h == 720);
        assert!(x + w <= 1280 && y == 0);
    }
}
