//! Golden tests for the GPU transform: known scRGB in, known sRGB codes out,
//! and agreement with the CPU reference in `color.rs`. They run on WARP, so
//! they are deterministic on any machine, and on the hardware adapter when
//! there is one.
#![cfg(windows)]

use capture_spike::{
    color::{self, Highlights},
    gpu::{Gpu, SdrConverter},
};
use half::f16;

fn devices() -> Vec<Gpu> {
    let mut devices = vec![Gpu::warp().expect("WARP device")];
    match Gpu::hardware(None) {
        Ok(gpu) => devices.push(gpu),
        Err(e) => eprintln!("no hardware D3D11 device, testing WARP only: {e}"),
    }
    devices
}

/// FP16-quantize scRGB pixels, as capture delivers them, returning both the
/// upload bytes and the quantized values the CPU reference should see.
fn fp16(pixels: &[[f32; 4]]) -> (Vec<u8>, Vec<[f32; 4]>) {
    let quantized: Vec<[f32; 4]> = pixels
        .iter()
        .map(|p| p.map(|v| f16::from_f32(v).to_f32()))
        .collect();
    let bytes = quantized
        .iter()
        .flatten()
        .flat_map(|v| f16::from_f32(*v).to_le_bytes())
        .collect();
    (bytes, quantized)
}

fn run(
    gpu: &Gpu,
    width: u32,
    height: u32,
    pixels: &[[f32; 4]],
    s: f32,
    mode: Highlights,
) -> (Vec<u8>, Vec<u8>) {
    let (bytes, quantized) = fp16(pixels);
    let texture = gpu.upload_rgba16f(width, height, &bytes).unwrap();
    let converter = SdrConverter::new(gpu).unwrap();
    let got = converter.convert(gpu, &texture, s, mode).unwrap().rgba;
    let want = color::convert(&quantized, width, height, s, mode);
    (got, want)
}

#[test]
fn sdr_ramps_come_back_exactly_at_every_white_level() {
    for gpu in devices() {
        for s in [1.0f32, 1.25, 1.5, 2.4, 3.0, 3.5, 6.0] {
            // 256 columns (one per code) × 4 rows: neutral, red, green, blue.
            let mut pixels = Vec::new();
            for row in 0..4 {
                for code in 0..=255u8 {
                    let c = color::code_to_linear(code) * s;
                    let mut p = [0.0, 0.0, 0.0, 1.0];
                    match row {
                        0 => p[..3].copy_from_slice(&[c; 3]),
                        r => p[r - 1] = c,
                    }
                    pixels.push(p);
                }
            }
            for mode in [Highlights::Shoulder, Highlights::Clip] {
                let (got, _) = run(&gpu, 256, 4, &pixels, s, mode);
                for (i, px) in got.as_chunks::<4>().0.iter().enumerate() {
                    let (row, code) = (i / 256, (i % 256) as u8);
                    let want = match row {
                        0 => [code, code, code, 255],
                        1 => [code, 0, 0, 255],
                        2 => [0, code, 0, 255],
                        _ => [0, 0, code, 255],
                    };
                    assert_eq!(
                        *px, want,
                        "{} S={s} {mode:?} row={row} code={code}",
                        gpu.adapter_name
                    );
                }
            }
        }
    }
}

/// A deterministic mix of SDR, extended, negative and out-of-range values
/// with a few HDR blobs, so the local headroom map has real work to do.
fn mixed_scene(width: u32, height: u32, s: f32) -> Vec<[f32; 4]> {
    let mut state = 0x2545_f491_u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state as f32 / u32::MAX as f32
    };
    (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| {
            let blob = ((x as i32 - 40).pow(2) + (y as i32 - 30).pow(2)) < 300
                || (x > 150 && x < 170 && y > 50);
            let scale = if blob { 6.0 * s } else { s };
            let mut p = [next() * scale, next() * scale, next() * scale, 1.0];
            if (x + y) % 37 == 0 {
                p[0] = -0.2 * s;
            }
            p
        })
        .collect()
}

#[test]
fn gpu_matches_cpu_reference_on_mixed_content() {
    let (width, height) = (200, 90);
    for gpu in devices() {
        for s in [1.0f32, 2.5, 4.0] {
            let pixels = mixed_scene(width, height, s);
            for mode in [Highlights::Shoulder, Highlights::Clip] {
                let (got, want) = run(&gpu, width, height, &pixels, s, mode);
                let mut worst = 0u8;
                let mut off_by_one = 0usize;
                for (g, w) in got.iter().zip(&want) {
                    let d = g.abs_diff(*w);
                    worst = worst.max(d);
                    off_by_one += usize::from(d == 1);
                }
                assert!(
                    worst <= 1,
                    "{} S={s} {mode:?}: worst difference {worst}",
                    gpu.adapter_name
                );
                // Rounding at a code boundary may flip; systematic drift may not.
                assert!(
                    off_by_one * 1000 < got.len(),
                    "{} S={s} {mode:?}: {off_by_one} off-by-one",
                    gpu.adapter_name
                );
            }
        }
    }
}

#[test]
fn shoulder_keeps_highlight_texture_that_clip_flattens() {
    // A neutral ramp from SDR white to 4× SDR white across 256 columns.
    let s = 3.0;
    let pixels: Vec<[f32; 4]> = (0..256 * 16)
        .map(|i| {
            let v = s * (1.0 + 3.0 * (i % 256) as f32 / 255.0);
            [v, v, v, 1.0]
        })
        .collect();
    for gpu in devices() {
        let distinct = |mode| {
            let (got, _) = run(&gpu, 256, 16, &pixels, s, mode);
            let row: std::collections::BTreeSet<u8> = got[..256 * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| p[0])
                .collect();
            row.len()
        };
        assert_eq!(distinct(Highlights::Clip), 1);
        assert!(distinct(Highlights::Shoulder) >= 8, "{}", gpu.adapter_name);
    }
}

#[test]
fn negative_and_extended_values_have_golden_outputs() {
    let s = 2.0;
    let pixels = [
        [-0.2, 1.0, 1.0, 1.0], // out of gamut, normalizes to the (0, .473, .473) case
        [8.0, 4.0, 2.0, 1.0],  // extended, colored
        [65504.0, 0.0, 0.0, 1.0], // FP16 max
        [-1.0, -1.0, -1.0, 1.0], // negative luminance
    ];
    let row: Vec<[f32; 4]> = pixels.iter().cycle().take(64).copied().collect();
    for gpu in devices() {
        let (got, _) = run(&gpu, 64, 1, &row, s, Highlights::Clip);
        assert_eq!(&got[0..4], [0, 183, 183, 255], "{}", gpu.adapter_name);
        assert_eq!(&got[4..8], [255, 188, 137, 255], "{}", gpu.adapter_name);
        assert_eq!(&got[8..12], [255, 0, 0, 255], "{}", gpu.adapter_name);
        assert_eq!(&got[12..16], [0, 0, 0, 255], "{}", gpu.adapter_name);
    }
}

#[test]
fn sdr_content_beside_hdr_content_stays_exact() {
    // Left: an HDR ramp from 1× to 4× SDR white. Right, touching it: every
    // grey code, as Windows composes SDR content.
    let s = 3.0;
    let (width, height) = (64 + 256, 8);
    let pixels: Vec<[f32; 4]> = (0..width * height)
        .map(|i| {
            let x = i % width;
            let v = if x < 64 {
                s * (1.0 + 3.0 * x as f32 / 63.0)
            } else {
                color::code_to_linear((x - 64) as u8) * s
            };
            [v, v, v, 1.0]
        })
        .collect();
    for gpu in devices() {
        let (got, want) = run(&gpu, width, height, &pixels, s, Highlights::Shoulder);
        assert_eq!(got, want, "{}", gpu.adapter_name);
        // x = 64 borders the HDR ramp; from x = 65 on, every code is exact.
        for x in 65..width {
            let code = (x - 64) as u8;
            let i = ((height / 2) * width + x) as usize * 4;
            assert_eq!(
                &got[i..i + 3],
                [code; 3],
                "{} code {code}",
                gpu.adapter_name
            );
        }
        // The HDR ramp keeps its gradation.
        let ramp: std::collections::BTreeSet<u8> = (0..64).map(|x| got[x * 4]).collect();
        assert!(ramp.len() >= 8, "{}", gpu.adapter_name);
    }
}
