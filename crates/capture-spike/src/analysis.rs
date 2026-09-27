//! Measurements for the Milestone 0 gate:
//!
//! - `compare`: two 8-bit captures of the same screen, pixel for pixel, in
//!   CIEDE2000 and code values.
//! - `transfer`: an HDR-off 8-bit reference against an HDR-on FP16 source,
//!   giving the curve Windows actually used to place SDR codes in scRGB.
//! - `code_fit`: the same question without a reference. SDR content is made
//!   of 8-bit codes, so under the right curve and white level every FP16
//!   value decodes to a near-integer code.

use crate::color;
use crate::raw::RawFrame;
use anyhow::{Result, ensure};

/// Provisional pass thresholds, fixed before any measurement: mean ΔE00 at
/// most 0.5 and the 99th percentile at most 1.0 (about one just-noticeable
/// difference).
pub const PASS_MEAN_DE: f64 = 0.5;
pub const PASS_P99_DE: f64 = 1.0;

/// A rectangle in physical pixels relative to the captured monitor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Roi {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Roi {
    pub fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// Parse `X,Y,W,H`.
    pub fn parse(text: &str) -> Result<Self> {
        let v: Vec<u32> = text
            .split(',')
            .map(|p| p.trim().parse())
            .collect::<Result<_, _>>()?;
        ensure!(v.len() == 4, "a region is X,Y,WIDTH,HEIGHT");
        Ok(Self {
            x: v[0],
            y: v[1],
            width: v[2],
            height: v[3],
        })
    }

    pub fn check(&self, width: u32, height: u32) -> Result<()> {
        ensure!(
            self.width > 0
                && self.height > 0
                && self.x.checked_add(self.width).is_some_and(|r| r <= width)
                && self.y.checked_add(self.height).is_some_and(|b| b <= height),
            "region {self:?} does not fit a {width}x{height} image"
        );
        Ok(())
    }

    fn pixels(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        (self.y..self.y + self.height)
            .flat_map(move |y| (self.x..self.x + self.width).map(move |x| (x, y)))
    }
}

// ---------------------------------------------------------------- ΔE2000

/// CIE L*a*b* (D65) of an 8-bit sRGB color.
pub fn srgb8_to_lab(rgb: [u8; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(|c| color::code_to_linear(c) as f64);
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = 0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b;
    let f = |t: f64| {
        const D: f64 = 6.0 / 29.0;
        if t > D * D * D {
            t.cbrt()
        } else {
            t / (3.0 * D * D) + 4.0 / 29.0
        }
    };
    let (fx, fy, fz) = (f(x / 0.950_47), f(y), f(z / 1.088_83));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIEDE2000 color difference (Sharma, Wu and Dalal, 2005).
pub fn delta_e2000(lab1: [f64; 3], lab2: [f64; 3]) -> f64 {
    let [l1, a1, b1] = lab1;
    let [l2, a2, b2] = lab2;
    let pow7 = |v: f64| v.powi(7);
    let c_bar = ((a1.hypot(b1)) + (a2.hypot(b2))) / 2.0;
    let g = 0.5 * (1.0 - (pow7(c_bar) / (pow7(c_bar) + pow7(25.0))).sqrt());
    let (a1p, a2p) = ((1.0 + g) * a1, (1.0 + g) * a2);
    let (c1p, c2p) = (a1p.hypot(b1), a2p.hypot(b2));
    let hue = |b: f64, a: f64| {
        if a == 0.0 && b == 0.0 {
            0.0
        } else {
            b.atan2(a).to_degrees().rem_euclid(360.0)
        }
    };
    let (h1p, h2p) = (hue(b1, a1p), hue(b2, a2p));
    let dl = l2 - l1;
    let dc = c2p - c1p;
    let dh = if c1p * c2p == 0.0 {
        0.0
    } else if (h2p - h1p).abs() <= 180.0 {
        h2p - h1p
    } else if h2p - h1p > 180.0 {
        h2p - h1p - 360.0
    } else {
        h2p - h1p + 360.0
    };
    let d_hue = 2.0 * (c1p * c2p).sqrt() * (dh.to_radians() / 2.0).sin();
    let l_bar = (l1 + l2) / 2.0;
    let c_bar_p = (c1p + c2p) / 2.0;
    let h_bar = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) / 2.0
    } else {
        (h1p + h2p - 360.0) / 2.0
    };
    let cos = |deg: f64| deg.to_radians().cos();
    let t =
        1.0 - 0.17 * cos(h_bar - 30.0) + 0.24 * cos(2.0 * h_bar) + 0.32 * cos(3.0 * h_bar + 6.0)
            - 0.20 * cos(4.0 * h_bar - 63.0);
    let d_theta = 30.0 * (-((h_bar - 275.0) / 25.0).powi(2)).exp();
    let r_c = 2.0 * (pow7(c_bar_p) / (pow7(c_bar_p) + pow7(25.0))).sqrt();
    let s_l = 1.0 + 0.015 * (l_bar - 50.0).powi(2) / (20.0 + (l_bar - 50.0).powi(2)).sqrt();
    let s_c = 1.0 + 0.045 * c_bar_p;
    let s_h = 1.0 + 0.015 * c_bar_p * t;
    let r_t = -(2.0 * d_theta).to_radians().sin() * r_c;
    let (l, c, h) = (dl / s_l, dc / s_c, d_hue / s_h);
    (l * l + c * c + h * h + r_t * c * h).sqrt()
}

// ---------------------------------------------------------------- compare

pub struct Block {
    pub x: u32,
    pub y: u32,
    pub mean_de: f64,
}

pub struct Comparison {
    pub roi: Roi,
    pub compared: u64,
    pub excluded: u64,
    pub mean_de: f64,
    pub p99_de: f64,
    pub p999_de: f64,
    pub max_de: f64,
    pub mean_code_difference: f64,
    pub max_code_difference: u8,
    pub channels_over_2: f64,
    /// The 64×64 blocks with the highest mean ΔE00, worst first.
    pub worst_blocks: Vec<Block>,
    /// ΔE00 per pixel (0 outside the region or where excluded).
    pub delta_e: Vec<f32>,
}

impl Comparison {
    pub fn passes(&self) -> bool {
        self.compared > 0 && self.mean_de <= PASS_MEAN_DE && self.p99_de <= PASS_P99_DE
    }

    pub fn summary(&self) -> String {
        format!(
            "{} over {} px ({} excluded): ΔE00 mean {:.3}, p99 {:.3}, p99.9 {:.3}, max {:.2}; \
             code diff mean {:.3}, max {}; channels >2 codes {:.3}%",
            if self.passes() { "PASS" } else { "FAIL" },
            self.compared,
            self.excluded,
            self.mean_de,
            self.p99_de,
            self.p999_de,
            self.max_de,
            self.mean_code_difference,
            self.max_code_difference,
            self.channels_over_2,
        )
    }
}

const BLOCK: u32 = 64;
const HISTOGRAM_STEP: f64 = 0.01;

/// Compare two RGBA8 images of the same size over `roi`, skipping pixels
/// where `exclude` is true.
pub fn compare(
    reference: &[u8],
    test: &[u8],
    width: u32,
    height: u32,
    roi: Roi,
    exclude: Option<&[bool]>,
) -> Result<Comparison> {
    ensure!(
        reference.len() == test.len() && reference.len() == (width * height * 4) as usize,
        "images differ in size; compare captures of the same monitor mode"
    );
    roi.check(width, height)?;
    let mut histogram = vec![0u64; 10_001];
    let blocks_x = width.div_ceil(BLOCK);
    let mut block_sums = vec![(0.0f64, 0u64); (blocks_x * height.div_ceil(BLOCK)) as usize];
    let mut delta_e = vec![0.0f32; (width * height) as usize];
    let (mut compared, mut excluded) = (0u64, 0u64);
    let (mut sum_de, mut max_de) = (0.0f64, 0.0f64);
    let (mut sum_code, mut max_code, mut over_2) = (0u64, 0u8, 0u64);
    let mut cache = std::collections::HashMap::<([u8; 3], [u8; 3]), f64>::new();
    for (x, y) in roi.pixels() {
        let i = (y * width + x) as usize;
        if exclude.is_some_and(|e| e[i]) {
            excluded += 1;
            continue;
        }
        let a = [reference[i * 4], reference[i * 4 + 1], reference[i * 4 + 2]];
        let b = [test[i * 4], test[i * 4 + 1], test[i * 4 + 2]];
        let de = if a == b {
            0.0
        } else {
            *cache
                .entry((a, b))
                .or_insert_with(|| delta_e2000(srgb8_to_lab(a), srgb8_to_lab(b)))
        };
        for c in 0..3 {
            let d = a[c].abs_diff(b[c]);
            sum_code += d as u64;
            max_code = max_code.max(d);
            over_2 += u64::from(d > 2);
        }
        compared += 1;
        sum_de += de;
        max_de = max_de.max(de);
        histogram[((de / HISTOGRAM_STEP) as usize).min(10_000)] += 1;
        delta_e[i] = de as f32;
        let block = &mut block_sums[((y / BLOCK) * blocks_x + x / BLOCK) as usize];
        block.0 += de;
        block.1 += 1;
    }
    let percentile = |p: f64| {
        let target = (p * compared as f64).ceil() as u64;
        let mut seen = 0;
        for (bin, count) in histogram.iter().enumerate() {
            seen += count;
            if seen >= target.max(1) {
                // Upper bin edge, so the figure is never optimistic; the
                // first bin reads 0 so identical images report 0.
                return if bin == 0 {
                    0.0
                } else {
                    (bin + 1) as f64 * HISTOGRAM_STEP
                };
            }
        }
        0.0
    };
    let mut worst_blocks: Vec<Block> = block_sums
        .iter()
        .enumerate()
        .filter(|(_, (sum, n))| *n > 0 && *sum > 0.0)
        .map(|(i, (sum, n))| Block {
            x: (i as u32 % blocks_x) * BLOCK,
            y: (i as u32 / blocks_x) * BLOCK,
            mean_de: sum / *n as f64,
        })
        .collect();
    worst_blocks.sort_by(|a, b| b.mean_de.total_cmp(&a.mean_de));
    worst_blocks.truncate(5);
    let n = compared.max(1) as f64;
    Ok(Comparison {
        roi,
        compared,
        excluded,
        mean_de: sum_de / n,
        p99_de: if compared > 0 { percentile(0.99) } else { 0.0 },
        p999_de: if compared > 0 { percentile(0.999) } else { 0.0 },
        max_de,
        mean_code_difference: sum_code as f64 / (3.0 * n),
        max_code_difference: max_code,
        channels_over_2: 100.0 * over_2 as f64 / (3.0 * n),
        worst_blocks,
        delta_e,
    })
}

/// A picture of where two captures differ: the reference dimmed to a
/// quarter, red where ΔE00 is large (full red at ΔE 4), blue where excluded.
pub fn heatmap(reference: &[u8], comparison: &Comparison, exclude: Option<&[bool]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(reference.len());
    for (i, px) in reference.as_chunks::<4>().0.iter().enumerate() {
        let grey = ((px[0] as u32 + px[1] as u32 + px[2] as u32) / 12) as u8;
        if exclude.is_some_and(|e| e[i]) {
            out.extend_from_slice(&[grey, grey, grey.saturating_add(96), 255]);
        } else {
            let red = (comparison.delta_e[i] * 64.0).min(255.0) as u8;
            out.extend_from_slice(&[grey.max(red), grey, grey, 255]);
        }
    }
    out
}

/// Pixels the SDR comparison must skip: those the shoulder changes, which
/// is HDR and other non-SDR content in a frame that holds values above SDR
/// white. There the HDR-off reference shows the application's own tone
/// mapping, which a desktop capture cannot reproduce. Empty otherwise.
pub fn shoulder_mask(raw: &RawFrame) -> (Vec<bool>, u64) {
    let map = raw.analysis();
    let mut mask = vec![false; (raw.width * raw.height) as usize];
    let mut count = 0;
    if map.has_extended() {
        for y in 0..raw.height {
            for x in 0..raw.width {
                if map.shoulder_applies(x, y) {
                    mask[(y * raw.width + x) as usize] = true;
                    count += 1;
                }
            }
        }
    }
    (mask, count)
}

// ---------------------------------------------------------------- transfer

/// A candidate for how Windows places an 8-bit SDR code in scRGB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    /// Piecewise sRGB (IEC 61966-2-1): what Framecut inverts.
    Srgb,
    /// Pure power 2.2, what many SDR displays actually do.
    Gamma22,
}

impl Curve {
    pub fn decode(self, code: f64) -> f64 {
        match self {
            Curve::Srgb => color::srgb_decode(code as f32) as f64,
            Curve::Gamma22 => code.max(0.0).powf(2.2),
        }
    }

    /// Normalized linear value → fractional 8-bit code.
    pub fn encode(self, linear: f64) -> f64 {
        let l = linear.max(0.0);
        255.0
            * match self {
                Curve::Srgb => {
                    if l <= 0.003_130_8 {
                        12.92 * l
                    } else {
                        1.055 * l.powf(1.0 / 2.4) - 0.055
                    }
                }
                Curve::Gamma22 => l.powf(1.0 / 2.2),
            }
    }

    pub fn name(self) -> &'static str {
        match self {
            Curve::Srgb => "piecewise sRGB",
            Curve::Gamma22 => "gamma 2.2",
        }
    }
}

pub struct CodeRow {
    pub code: u8,
    pub count: usize,
    /// Median FP16 value of this code's pixels, divided by S.
    pub measured: f64,
}

pub struct Transfer {
    pub white_scale: f32,
    pub rows: Vec<CodeRow>,
    /// Median scRGB of pure red, green and blue reference pixels, over S.
    pub primaries: [Option<[f64; 3]>; 3],
}

impl Transfer {
    /// Worst |implied code − reference code| under a curve, over codes with
    /// at least `min_count` pixels.
    pub fn worst_error(&self, curve: Curve, min_count: usize) -> Option<(u8, f64)> {
        self.rows
            .iter()
            .filter(|r| r.count >= min_count)
            .map(|r| (r.code, curve.encode(r.measured) - r.code as f64))
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
    }

    /// scRGB of SDR white as measured, over the S Windows reported.
    pub fn white_ratio(&self) -> Option<f64> {
        self.rows
            .iter()
            .find(|r| r.code == 255 && r.count > 0)
            .map(|r| r.measured)
    }

    /// Findings and a table of representative codes, as Markdown.
    pub fn markdown(&self) -> String {
        const MIN: usize = 50;
        let mut text = format!(
            "- S reported by Windows: {}\n- measured SDR white / S: {}\n",
            self.white_scale,
            self.white_ratio()
                .map_or("no #FFFFFF pixels".into(), |r| format!("{r:.5}"))
        );
        for curve in [Curve::Srgb, Curve::Gamma22] {
            if let Some((code, error)) = self.worst_error(curve, MIN) {
                text += &format!(
                    "- {}: worst implied-code error {error:+.2} (at code {code}; codes with ≥{MIN} px)\n",
                    curve.name()
                );
            }
        }
        for (name, p) in ["red", "green", "blue"].iter().zip(&self.primaries) {
            if let Some([r, g, b]) = p {
                text += &format!("- pure {name} measures ({r:.4}, {g:.4}, {b:.4}) × S\n");
            }
        }
        text += "\n| code | pixels | measured / S | piecewise sRGB | gamma 2.2 | implied code (sRGB) |\n";
        text += "|---:|---:|---:|---:|---:|---:|\n";
        const SHOWN: [u8; 22] = [
            1, 2, 4, 8, 16, 24, 32, 48, 64, 96, 118, 128, 160, 192, 204, 224, 238, 240, 247, 250,
            254, 255,
        ];
        for row in self.rows.iter().filter(|r| SHOWN.contains(&r.code)) {
            let c = row.code as f64 / 255.0;
            text += &format!(
                "| {} | {} | {:.5} | {:.5} | {:.5} | {:.2} |\n",
                row.code,
                row.count,
                row.measured,
                Curve::Srgb.decode(c),
                Curve::Gamma22.decode(c),
                Curve::Srgb.encode(row.measured)
            );
        }
        text
    }
}

fn median(values: &mut [f32]) -> f64 {
    let mid = values.len() / 2;
    *values.select_nth_unstable_by(mid, f32::total_cmp).1 as f64
}

/// For every neutral code in the 8-bit `reference`, the median value the
/// HDR `raw` frame holds at the same pixels, over `roi`.
pub fn transfer(reference: &[u8], raw: &RawFrame, roi: Roi) -> Result<Transfer> {
    ensure!(
        reference.len() == (raw.width * raw.height * 4) as usize,
        "the reference and the FP16 frame differ in size"
    );
    roi.check(raw.width, raw.height)?;
    let s = raw.white_scale;
    let mut by_code: Vec<Vec<f32>> = vec![Vec::new(); 256];
    let mut primaries: [Vec<[f32; 3]>; 3] = Default::default();
    for (x, y) in roi.pixels() {
        let i = (y * raw.width + x) as usize * 4;
        let [r, g, b] = [reference[i], reference[i + 1], reference[i + 2]];
        let p = raw.pixel(x, y);
        if r == g && g == b {
            by_code[r as usize].extend([p[0] / s, p[1] / s, p[2] / s]);
        } else {
            for (c, want) in [[255, 0, 0], [0, 255, 0], [0, 0, 255]].iter().enumerate() {
                if [r, g, b] == *want {
                    primaries[c].push([p[0] / s, p[1] / s, p[2] / s]);
                }
            }
        }
    }
    let rows = by_code
        .iter_mut()
        .enumerate()
        .filter(|(_, v)| !v.is_empty())
        .map(|(code, values)| CodeRow {
            code: code as u8,
            count: values.len() / 3,
            measured: median(values),
        })
        .collect();
    let primaries = primaries.map(|samples| {
        (!samples.is_empty()).then(|| {
            std::array::from_fn(|c| median(&mut samples.iter().map(|p| p[c]).collect::<Vec<_>>()))
        })
    });
    Ok(Transfer {
        white_scale: s,
        rows,
        primaries,
    })
}

// ---------------------------------------------------------------- code fit

/// Where values sit off the 8-bit code grid under `curve`: per pixel, the
/// worst channel's distance from a code, from black (on a code) to full red
/// (half a code, as far as possible). Blue marks pixels above SDR white.
pub fn code_fit_map(raw: &RawFrame, curve: Curve) -> Vec<u8> {
    let s = raw.white_scale as f64;
    let mut out = Vec::with_capacity((raw.width * raw.height * 4) as usize);
    for p in raw.pixels() {
        let n = [p[0], p[1], p[2]].map(|v| v as f64 / s);
        if n.iter().any(|&v| v > 1.0 + color::EXTENDED_EPSILON as f64) {
            out.extend_from_slice(&[0, 0, 255, 255]);
            continue;
        }
        let worst = n
            .iter()
            .filter(|&&v| v > 0.0)
            .map(|&v| {
                let code = curve.encode(v);
                (code - code.round()).abs()
            })
            .fold(0.0f64, f64::max);
        out.extend_from_slice(&[(worst * 510.0).min(255.0) as u8, 0, 0, 255]);
    }
    out
}

pub fn code_fit_markdown(fits: &[CodeFit]) -> String {
    let mut text = String::from(
        "| curve | channel values | within 0.1 of a code | mean distance |\n|---|---:|---:|---:|\n",
    );
    for fit in fits {
        text += &format!(
            "| {} | {} | {:.2}% | {:.3} |\n",
            fit.curve.name(),
            fit.samples,
            fit.within_0_1,
            fit.mean_residual
        );
    }
    text + "\nChance alone puts 20% within 0.1 of a code, at a mean distance of 0.25.\n"
}

pub struct CodeFit {
    pub curve: Curve,
    /// Channel values considered (non-zero, at most SDR white).
    pub samples: u64,
    /// Share of values whose implied code is within 0.1 of an integer.
    /// Chance alone gives 20%.
    pub within_0_1: f64,
    pub mean_residual: f64,
}

/// How well each curve explains the FP16 values as 8-bit codes, with no
/// reference image. Only channels in `(0, 1]` after normalization count:
/// zero fits every curve, and above 1 is not SDR content.
pub fn code_fit(raw: &RawFrame, roi: Roi) -> Result<Vec<CodeFit>> {
    roi.check(raw.width, raw.height)?;
    // FP16 values repeat heavily in UI; tally distinct values first.
    let mut counts = std::collections::HashMap::<u16, u64>::new();
    for (x, y) in roi.pixels() {
        let i = (y as usize * raw.width as usize + x as usize) * 8;
        for c in 0..3 {
            let bits = u16::from_le_bytes([raw.data[i + 2 * c], raw.data[i + 2 * c + 1]]);
            *counts.entry(bits).or_default() += 1;
        }
    }
    let s = raw.white_scale as f64;
    Ok([Curve::Srgb, Curve::Gamma22]
        .into_iter()
        .map(|curve| {
            let (mut samples, mut close, mut residual) = (0u64, 0u64, 0.0f64);
            for (&bits, &n) in &counts {
                let v = half::f16::from_bits(bits).to_f64() / s;
                if !(v > 0.0 && v <= 1.0 + color::EXTENDED_EPSILON as f64) {
                    continue;
                }
                let code = curve.encode(v);
                let r = (code - code.round()).abs();
                samples += n;
                residual += r * n as f64;
                close += if r <= 0.1 { n } else { 0 };
            }
            CodeFit {
                curve,
                samples,
                within_0_1: 100.0 * close as f64 / samples.max(1) as f64,
                mean_residual: residual / samples.max(1) as f64,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::ColorMode;

    /// Reference pairs from Sharma, Wu and Dalal (2005), Table 1.
    #[test]
    fn ciede2000_matches_published_pairs() {
        let pairs = [
            ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
            ([50.0, 3.1571, -77.2803], [50.0, 0.0, -82.7485], 2.8615),
            ([50.0, 0.0, 0.0], [50.0, -1.0, 2.0], 2.3669),
            ([50.0, 2.49, -0.001], [50.0, -2.49, 0.0009], 7.1792),
            ([50.0, 2.5, 0.0], [73.0, 25.0, -18.0], 27.1492),
            (
                [60.2574, -34.0099, 36.2677],
                [60.4626, -34.1751, 39.4387],
                1.2644,
            ),
        ];
        for (a, b, want) in pairs {
            let got = delta_e2000(a, b);
            assert!(
                (got - want).abs() < 1e-4,
                "{a:?} {b:?}: got {got}, want {want}"
            );
            assert!((delta_e2000(b, a) - want).abs() < 1e-4);
        }
    }

    #[test]
    fn lab_of_known_srgb_colors() {
        let close = |a: [f64; 3], b: [f64; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.05);
        assert!(close(srgb8_to_lab([255, 255, 255]), [100.0, 0.0, 0.0]));
        assert!(close(srgb8_to_lab([0, 0, 0]), [0.0, 0.0, 0.0]));
        assert!(close(srgb8_to_lab([255, 0, 0]), [53.24, 80.09, 67.20]));
        assert!(close(srgb8_to_lab([119, 119, 119]), [50.03, 0.0, 0.0]));
    }

    fn solid(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
        (0..width * height)
            .flat_map(|_| [rgb[0], rgb[1], rgb[2], 255])
            .collect()
    }

    #[test]
    fn identical_images_pass_and_different_ones_fail() {
        let a = solid(8, 8, [255, 255, 255]);
        let same = compare(&a, &a, 8, 8, Roi::full(8, 8), None).unwrap();
        assert!(same.passes());
        assert_eq!(same.max_de, 0.0);
        let grey = solid(8, 8, [235, 235, 235]);
        let washed = compare(&a, &grey, 8, 8, Roi::full(8, 8), None).unwrap();
        assert!(!washed.passes());
        assert_eq!(washed.max_code_difference, 20);
        // Excluding every differing pixel makes it pass again.
        let all = vec![true; 64];
        let masked = compare(&a, &grey, 8, 8, Roi::full(8, 8), Some(&all)).unwrap();
        assert_eq!((masked.compared, masked.excluded), (0, 64));
    }

    /// Rounding at a code boundary may flip a few pixels by one code; that
    /// passes. A systematic tint of the same size is a model error and fails,
    /// although it is below one just-noticeable difference.
    #[test]
    fn sporadic_rounding_passes_but_a_systematic_tint_fails() {
        let a = solid(16, 16, [200, 200, 200]);
        let mut b = a.clone();
        for px in b.as_chunks_mut::<4>().0.iter_mut().step_by(10) {
            px[0] = 201;
        }
        let c = compare(&a, &b, 16, 16, Roi::full(16, 16), None).unwrap();
        assert!(c.passes(), "{}", c.summary());
        assert_eq!(c.max_code_difference, 1);

        let tinted = solid(16, 16, [201, 200, 199]);
        let c = compare(&a, &tinted, 16, 16, Roi::full(16, 16), None).unwrap();
        assert!(!c.passes(), "{}", c.summary());
        assert!(c.max_de < 1.0);
    }

    fn raw_of(width: u32, height: u32, s: f32, value: impl Fn(u32) -> [f32; 3]) -> RawFrame {
        let data = (0..width * height)
            .flat_map(|i| {
                let [r, g, b] = value(i);
                [r, g, b, 1.0].map(half::f16::from_f32)
            })
            .flat_map(|v| v.to_le_bytes())
            .collect();
        RawFrame {
            width,
            height,
            white_scale: s,
            color_mode: ColorMode::Hdr,
            data,
        }
    }

    #[test]
    fn transfer_recovers_the_curve_windows_used() {
        let s = 2.5;
        let reference: Vec<u8> = (0..256u32)
            .flat_map(|c| [c as u8, c as u8, c as u8, 255])
            .collect();
        let srgb = raw_of(256, 1, s, |i| [color::code_to_linear(i as u8) * s; 3]);
        let t = transfer(&reference, &srgb, Roi::full(256, 1)).unwrap();
        assert_eq!(t.rows.len(), 256);
        assert!(t.worst_error(Curve::Srgb, 1).unwrap().1.abs() < 0.2);
        assert!(t.worst_error(Curve::Gamma22, 1).unwrap().1.abs() > 2.0);
        assert!((t.white_ratio().unwrap() - 1.0).abs() < 1e-3);

        let gamma = raw_of(256, 1, s, |i| [((i as f32 / 255.0).powf(2.2)) * s; 3]);
        let t = transfer(&reference, &gamma, Roi::full(256, 1)).unwrap();
        assert!(t.worst_error(Curve::Gamma22, 1).unwrap().1.abs() < 0.2);
        assert!(t.worst_error(Curve::Srgb, 1).unwrap().1.abs() > 2.0);
    }

    #[test]
    fn code_fit_tells_the_curves_apart() {
        let s = 3.0;
        let srgb = raw_of(256, 1, s, |i| [color::code_to_linear(i as u8) * s; 3]);
        let fits = code_fit(&srgb, Roi::full(256, 1)).unwrap();
        assert!(fits[0].within_0_1 > 99.0, "sRGB {}", fits[0].within_0_1);
        assert!(fits[1].within_0_1 < 60.0, "2.2 {}", fits[1].within_0_1);
        // A wrong white scale spoils the fit too.
        let wrong = raw_of(256, 1, s, |i| [color::code_to_linear(i as u8) * s * 1.1; 3]);
        assert!(code_fit(&wrong, Roi::full(256, 1)).unwrap()[0].within_0_1 < 60.0);
    }

    #[test]
    fn regions_parse_and_are_checked() {
        let r = Roi::parse("10, 20,30,40").unwrap();
        assert_eq!((r.x, r.y, r.width, r.height), (10, 20, 30, 40));
        assert!(r.check(40, 60).is_ok());
        assert!(r.check(39, 60).is_err());
        assert!(Roi::parse("1,2,3").is_err());
    }
}
