//! Framecut's HDR/WCG → SDR transform, CPU reference.
//!
//! The production path is the GPU shader in `shaders/hdr_to_sdr.hlsl`. This
//! module mirrors it step for step so tests can hold the shader to known
//! values, and so analysis commands can reason about captures. Every constant
//! is derived in `docs/COLOR_PIPELINE.md`.
//!
//! Input is linear scRGB (BT.709 primaries, D65, 1.0 = 80 nits in HDR mode),
//! as Windows.Graphics.Capture delivers it in `R16G16B16A16_FLOAT`. Output is
//! 8-bit sRGB.

use anyhow::{Result, bail};

/// Recorded in capture reports so a PNG can be traced to the math that made it.
pub const TRANSFORM_VERSION: &str = "sdr-codes-exact-shoulder-v3";

/// Side of the square tiles the GPU reduces the frame peak over, in pixels.
pub const TILE: u32 = 16;
/// Normalized values up to `1 + EXTENDED_EPSILON` are SDR white. This absorbs
/// FP16 rounding of `S × 1.0` (at most 2⁻¹¹ relative) with margin, so SDR
/// white never registers as an HDR highlight.
pub const EXTENDED_EPSILON: f32 = 1.0 / 256.0;
/// Fraction of the SDR range the shoulder may borrow as headroom grows
/// without bound. See `shoulder`.
pub const SHOULDER_BETA: f32 = 0.25;
/// BT.709 / sRGB relative luminance weights.
pub const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];
/// Largest finite FP16 value; scRGB input is clamped to it.
const FP16_MAX: f32 = 65504.0;

/// How values above SDR white are brought into range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Highlights {
    /// Default. SDR content is reproduced exactly; when the frame holds
    /// values above SDR white, all other content goes through a shoulder.
    Shoulder,
    /// Diagnostic. Every pixel is projected onto the SDR cube along its RGB
    /// ray. This is the prior spike's behaviour: neutral highlights go flat white.
    Clip,
}

impl Highlights {
    pub fn name(self) -> &'static str {
        match self {
            Highlights::Shoulder => "shoulder",
            Highlights::Clip => "clip",
        }
    }

    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "shoulder" => Ok(Highlights::Shoulder),
            "clip" => Ok(Highlights::Clip),
            _ => bail!("unknown highlight mode {name:?}; expected shoulder or clip"),
        }
    }
}

/// How the Windows compositor is driving a display. Mirrors
/// `DISPLAYCONFIG_ADVANCED_COLOR_MODE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorMode {
    /// 8-bit sRGB composition. Not Advanced Color.
    Sdr,
    /// Advanced Color on an SDR display: FP16 scRGB composition with
    /// display-referred luminance, so scRGB 1.0 is SDR white.
    Wcg,
    /// Advanced Color with HDR: FP16 scRGB composition with scene-referred
    /// luminance, so scRGB 1.0 is 80 nits and SDR white sits at `S`.
    Hdr,
}

impl ColorMode {
    pub fn name(self) -> &'static str {
        match self {
            ColorMode::Sdr => "SDR",
            ColorMode::Wcg => "WCG (Advanced Color SDR)",
            ColorMode::Hdr => "HDR",
        }
    }

    pub fn advanced_color(self) -> bool {
        self != ColorMode::Sdr
    }
}

/// The scRGB value of SDR white, `S`, for a display.
///
/// `sdr_white_level` is `DISPLAYCONFIG_SDR_WHITE_LEVEL::SDRWhiteLevel`, a
/// fixed-point multiple of 80 nits where 1000 means 80 nits. Only HDR mode
/// scales SDR content by it. WCG composition is display-referred, and SDR
/// composition has no scRGB stage, so both use 1.
pub fn white_scale(mode: ColorMode, sdr_white_level: Option<u32>) -> Result<f32> {
    match mode {
        ColorMode::Hdr => match sdr_white_level {
            Some(level) if level > 0 => Ok(level as f32 / 1000.0),
            _ => bail!("HDR display reported no SDR white level; refusing to guess one"),
        },
        ColorMode::Wcg | ColorMode::Sdr => Ok(1.0),
    }
}

/// SDR white in nits for a raw `SDRWhiteLevel`.
pub fn sdr_white_nits(sdr_white_level: u32) -> f32 {
    sdr_white_level as f32 * 80.0 / 1000.0
}

/// sRGB electro-optical transfer: encoded value → linear light. IEC 61966-2-1.
pub fn srgb_decode(e: f32) -> f32 {
    if e <= 0.04045 {
        e / 12.92
    } else {
        ((e + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse of `srgb_decode`, for linear values in `[0, 1]`.
pub fn srgb_encode(l: f32) -> f32 {
    let l = l.clamp(0.0, 1.0);
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// Round an encoded value in `[0, 1]` to an 8-bit code. The shader does the
/// same arithmetic rather than relying on UNORM conversion, whose rounding
/// the D3D11 spec only bounds to 0.6 ULP.
pub fn quantize(e: f32) -> u8 {
    (e.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u8
}

/// Linear value of an 8-bit sRGB code.
pub fn code_to_linear(code: u8) -> f32 {
    srgb_decode(code as f32 / 255.0)
}

fn luminance(c: [f32; 3]) -> f32 {
    c[0] * LUMA[0] + c[1] * LUMA[1] + c[2] * LUMA[2]
}

fn max3(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2])
}

/// Step 1 and 2: divide by SDR white, then bring colors outside the sRGB
/// gamut (negative scRGB components) back inside by desaturating toward
/// their own luminance. Luminance and hue angle are kept; only chroma drops,
/// and only as far as needed to make the smallest channel zero.
pub fn normalize(c: [f32; 3], white_scale: f32) -> [f32; 3] {
    let n = c.map(|v| {
        if v.is_nan() {
            0.0
        } else {
            v.clamp(-FP16_MAX, FP16_MAX) / white_scale
        }
    });
    let lo = n[0].min(n[1]).min(n[2]);
    if lo >= 0.0 {
        return n;
    }
    let y = luminance(n);
    if y <= 0.0 {
        return [0.0; 3];
    }
    let t = y / (y - lo);
    n.map(|v| (y + t * (v - y)).max(0.0))
}

/// Peak channel of a normalized pixel. This is what the highlight analysis
/// measures: a pixel is extended when its peak exceeds SDR white.
pub fn peak(n: [f32; 3]) -> f32 {
    max3(n)
}

/// Knee of the shoulder for local headroom `h` (> 1): `1 − β(1 − 1/h)`.
/// It starts at 1 when there is no headroom and falls toward `1 − β`.
pub fn knee(h: f32) -> f32 {
    1.0 - SHOULDER_BETA * (1.0 - 1.0 / h)
}

/// Compress `[0, h]` into `[0, 1]`: identity up to the knee `k`, then an
/// extended-Reinhard shoulder that leaves the knee with slope 1 and lands
/// exactly on 1 at `h`. Monotonic; tends to identity as `h → 1`.
pub fn shoulder(x: f32, h: f32) -> f32 {
    let k = knee(h);
    if x <= k {
        return x;
    }
    let x = x.min(h);
    let a = (x - k) / (1.0 - k);
    let big_a = (h - k) / (1.0 - k);
    k + (1.0 - k) * (a * (1.0 + a / (big_a * big_a)) / (1.0 + a))
}

/// Distance from an integer 8-bit code within which a channel counts as an
/// SDR code value. Near white one FP16 step is about 0.073 of a code (less
/// lower down), and applications that composite SDR content in their own
/// FP16 pipelines (Edge, with HDR content on the page) land up to two steps
/// off the grid, so this allows about three. Content that is not 8-bit SDR
/// (HDR, video, compositor blending) lands this close only by chance, half
/// the time per checked channel.
pub const CODE_TOLERANCE: f32 = 0.25;
/// Channels closer to zero than this (normalized; about code 18) are not
/// checked against the grid. Applications that convert colors in FP16 leave
/// residue of about ±0.0001 in channels that should be 0 (Edge puts code 0.4
/// of red into pure green), and a channel this dark is far below the
/// shoulder's knee, so whether it is SDR content never changes the result.
pub const NEGLIGIBLE: f32 = 0.005;
/// Cross-talk between channels, as a share of the pixel's peak, still read as
/// an SDR code. Edge's FP16 color conversion moves a dim channel beside a
/// bright one by up to about 0.0004 × the bright one (red codes 22–61 on a
/// green gradient sit 0.27–0.37 code off the grid).
pub const CROSS_TALK: f32 = 0.001;

/// Whether every channel of a captured scRGB pixel is an SDR code value:
/// within SDR white, and either negligible, or `S × decode(code)` for some
/// 8-bit code to within `CODE_TOLERANCE` codes or `CROSS_TALK` × the pixel's
/// peak in linear light.
pub fn on_code_grid(c: [f32; 3], white_scale: f32) -> bool {
    let n = c.map(|v| v / white_scale);
    let top = n[0].max(n[1]).max(n[2]);
    n.iter().all(|&n| {
        if n.abs() < NEGLIGIBLE {
            return true;
        }
        if !(0.0..=1.0 + EXTENDED_EPSILON).contains(&n) {
            return false;
        }
        let code = srgb_encode(n) * 255.0;
        let nearest = code.round();
        (code - nearest).abs() <= CODE_TOLERANCE
            || (n - srgb_decode(nearest / 255.0)).abs() <= CROSS_TALK * top
    })
}

/// What `tone_map` needs to know about a pixel's frame and surroundings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PixelContext {
    /// Peak of the whole frame over SDR white: the headroom the shoulder
    /// maps to 1.
    pub frame_peak: f32,
    /// The pixel and its eight neighbours are all SDR code values, so it is
    /// ordinary SDR content, which is reproduced exactly.
    pub sdr_content: bool,
}

impl PixelContext {
    /// Ordinary SDR content: left as it is.
    pub const SDR: Self = Self {
        frame_peak: 1.0,
        sdr_content: true,
    };
}

/// Step 3 and 4: bring a normalized pixel into the SDR cube. Returns linear
/// sRGB in `[0, 1]`.
///
/// When the frame holds values above SDR white, everything that is not SDR
/// content goes through one shoulder curve for the whole frame, so
/// highlights keep their order and texture. SDR content is never touched.
pub fn tone_map(n: [f32; 3], at: PixelContext, mode: Highlights) -> [f32; 3] {
    let mut n = n;
    if mode == Highlights::Shoulder {
        let m = peak(n);
        if at.frame_peak > 1.0 + EXTENDED_EPSILON && !at.sdr_content && m > 0.0 {
            let scale = shoulder(m, at.frame_peak) / m;
            n = n.map(|v| v * scale);
        }
    }
    let m = peak(n).max(1.0);
    n.map(|v| v / m)
}

/// Encode a tone-mapped pixel as 8-bit sRGB.
pub fn encode_pixel(l: [f32; 3]) -> [u8; 3] {
    l.map(|v| quantize(srgb_encode(v)))
}

/// The per-frame analysis the shader does before converting: the frame's
/// peak, and which pixels are SDR code values.
pub struct FrameAnalysis {
    pub width: u32,
    pub height: u32,
    pub frame_peak: f32,
    pub on_grid: Vec<bool>,
}

impl FrameAnalysis {
    pub fn new(scrgb: &[[f32; 4]], width: u32, height: u32, white_scale: f32) -> Self {
        let mut frame_peak = 0.0f32;
        let mut on_grid = Vec::with_capacity(scrgb.len());
        for p in scrgb {
            let c = [p[0], p[1], p[2]];
            frame_peak = frame_peak.max(peak(normalize(c, white_scale)));
            on_grid.push(on_code_grid(c, white_scale));
        }
        Self {
            width,
            height,
            frame_peak,
            on_grid,
        }
    }

    /// The pixel and its eight neighbours (clamped at the edges) are all SDR
    /// code values. Requiring the neighbours makes chance matches inside HDR
    /// or video content rare: a bright, saturated HDR pixel has one channel
    /// that is really tested, so nine pixels must all match by chance.
    pub fn sdr_content(&self, x: u32, y: u32) -> bool {
        let (w, h) = (self.width, self.height);
        let (x0, x1) = (x.saturating_sub(1), (x + 1).min(w - 1));
        let (y0, y1) = (y.saturating_sub(1), (y + 1).min(h - 1));
        (y0..=y1).all(|y| (x0..=x1).all(|x| self.on_grid[(y * w + x) as usize]))
    }

    pub fn at(&self, x: u32, y: u32) -> PixelContext {
        PixelContext {
            frame_peak: self.frame_peak,
            sdr_content: self.sdr_content(x, y),
        }
    }

    /// Whether any pixel is brighter than SDR white.
    pub fn has_extended(&self) -> bool {
        self.frame_peak > 1.0 + EXTENDED_EPSILON
    }

    /// Whether the shoulder changes this pixel's treatment: the frame has
    /// HDR content and the pixel is not SDR content.
    pub fn shoulder_applies(&self, x: u32, y: u32) -> bool {
        self.has_extended() && !self.sdr_content(x, y)
    }
}

/// The whole transform on the CPU: scRGB in, RGBA8 sRGB out (opaque).
pub fn convert(
    scrgb: &[[f32; 4]],
    width: u32,
    height: u32,
    white_scale: f32,
    mode: Highlights,
) -> Vec<u8> {
    let analysis = FrameAnalysis::new(scrgb, width, height, white_scale);
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let p = scrgb[(y * width + x) as usize];
            let n = normalize([p[0], p[1], p[2]], white_scale);
            let [r, g, b] = encode_pixel(tone_map(n, analysis.at(x, y), mode));
            out.extend_from_slice(&[r, g, b, 255]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, tolerance: f32) -> bool {
        (a - b).abs() <= tolerance
    }

    /// A pixel on HDR content in a frame whose peak is `frame_peak`.
    fn on(frame_peak: f32) -> PixelContext {
        PixelContext {
            frame_peak,
            sdr_content: false,
        }
    }

    #[test]
    fn white_scale_follows_the_queried_level_in_hdr_only() {
        assert_eq!(white_scale(ColorMode::Hdr, Some(1000)).unwrap(), 1.0);
        assert_eq!(white_scale(ColorMode::Hdr, Some(2500)).unwrap(), 2.5);
        assert_eq!(white_scale(ColorMode::Hdr, Some(3000)).unwrap(), 3.0);
        assert_eq!(white_scale(ColorMode::Hdr, Some(6000)).unwrap(), 6.0);
        assert!(white_scale(ColorMode::Hdr, Some(0)).is_err());
        assert!(white_scale(ColorMode::Hdr, None).is_err());
        // Advanced Color SDR is display-referred: whatever level Windows
        // reports, SDR white is scRGB 1.0.
        assert_eq!(white_scale(ColorMode::Wcg, Some(3000)).unwrap(), 1.0);
        assert_eq!(white_scale(ColorMode::Sdr, None).unwrap(), 1.0);
    }

    #[test]
    fn sdr_white_level_converts_to_nits() {
        assert_eq!(sdr_white_nits(1000), 80.0);
        assert_eq!(sdr_white_nits(3000), 240.0);
        assert_eq!(sdr_white_nits(6000), 480.0);
    }

    #[test]
    fn srgb_transfer_matches_known_values() {
        assert!(close(srgb_decode(0.5), 0.214_041, 1e-6));
        assert!(close(srgb_decode(0.04045), 0.003_130_8, 1e-7));
        assert!(close(srgb_encode(0.18), 0.461_356, 1e-5));
        assert_eq!(quantize(srgb_encode(0.18)), 118);
        assert_eq!(quantize(srgb_encode(1.0)), 255);
        assert_eq!(quantize(srgb_encode(0.0)), 0);
        for code in 0..=255u8 {
            assert_eq!(quantize(srgb_encode(code_to_linear(code))), code);
        }
    }

    /// Ordinary SDR content, as HDR Windows composes it (`S × decode(code)`),
    /// comes back as the original code at any SDR white level.
    #[test]
    fn sdr_codes_round_trip_at_every_white_level() {
        for s in [1.0, 1.25, 1.5, 2.4, 3.0, 3.5, 6.0] {
            for code in 0..=255u8 {
                let c = code_to_linear(code) * s;
                for pixel in [[c, c, c], [c, 0.0, 0.0], [0.0, c, 0.0], [0.0, 0.0, c]] {
                    let n = normalize(pixel, s);
                    let out = encode_pixel(tone_map(n, PixelContext::SDR, Highlights::Shoulder));
                    let want = pixel.map(|v| if v > 0.0 { code } else { 0 });
                    assert_eq!(out, want, "S={s} code={code} pixel={pixel:?}");
                }
            }
        }
    }

    #[test]
    fn known_scrgb_values_map_to_known_codes() {
        let px = |c: [f32; 3], s: f32| {
            encode_pixel(tone_map(
                normalize(c, s),
                PixelContext::SDR,
                Highlights::Shoulder,
            ))
        };
        // SDR white at 240 nits.
        assert_eq!(px([3.0, 3.0, 3.0], 3.0), [255, 255, 255]);
        // #808080 at S = 2.5: 2.5 × 0.215861 = 0.539653.
        assert_eq!(px([0.539_653; 3], 2.5), [128, 128, 128]);
        // #F7F7F7 at S = 3: decode(247/255) = 0.930111.
        assert_eq!(px([2.790_333; 3], 3.0), [247, 247, 247]);
        // Pure red at S = 1.5.
        assert_eq!(px([1.5, 0.0, 0.0], 1.5), [255, 0, 0]);
        // Black stays black.
        assert_eq!(px([0.0; 3], 4.0), [0, 0, 0]);
    }

    #[test]
    fn negative_scrgb_desaturates_toward_its_luminance() {
        // Y = 0.2126 × −0.1 + 0.7152 × 0.5 + 0.0722 × 0.5 = 0.37244.
        let n = normalize([-0.1, 0.5, 0.5], 1.0);
        assert!(close(n[0], 0.0, 1e-6));
        assert!(close(n[1], 0.473_000, 1e-5));
        assert!(close(n[2], 0.473_000, 1e-5));
        assert!(close(luminance(n), 0.37244, 1e-5));
        assert_eq!(
            encode_pixel(tone_map(n, PixelContext::SDR, Highlights::Shoulder)),
            [0, 183, 183]
        );
        // No positive luminance: black.
        assert_eq!(normalize([-0.5, 0.1, 0.0], 1.0), [0.0; 3]);
        // In-gamut colors are untouched.
        assert_eq!(normalize([0.2, 0.4, 0.6], 2.0), [0.1, 0.2, 0.3]);
    }

    #[test]
    fn non_finite_input_is_contained() {
        assert_eq!(normalize([f32::NAN, 0.5, 0.5], 1.0), [0.0, 0.5, 0.5]);
        let n = normalize([f32::INFINITY, 0.0, 0.0], 1.0);
        assert_eq!(
            encode_pixel(tone_map(n, on(n[0]), Highlights::Shoulder)),
            [255, 0, 0]
        );
    }

    #[test]
    fn shoulder_matches_hand_derived_values() {
        // h = 4: k = 1 − 0.25 × 0.75 = 0.8125, A = 17.
        assert!(close(knee(4.0), 0.8125, 1e-7));
        assert!(close(shoulder(0.5, 4.0), 0.5, 0.0));
        assert!(close(shoulder(1.0, 4.0), 0.906_574, 1e-5));
        assert!(close(shoulder(4.0, 4.0), 1.0, 1e-6));
        assert_eq!(quantize(srgb_encode(shoulder(1.0, 4.0))), 244);
        // Unbounded headroom: knee at 0.75, SDR white at 0.875.
        assert!(close(shoulder(1.0, 1e6), 0.875, 1e-4));
        // The codes docs/COLOR_PIPELINE.md quotes for SDR white.
        let white = |p: f32| quantize(srgb_encode(shoulder(1.0, p)));
        assert_eq!((white(2.0), white(4.0), white(1e6)), (248, 244, 240));
    }

    #[test]
    fn shoulder_is_monotonic_and_bounded() {
        for h in [1.01, 1.5, 2.0, 4.0, 10.0, 100.0] {
            let mut last = 0.0;
            for i in 0..=2000 {
                let x = h * i as f32 / 2000.0;
                let y = shoulder(x, h);
                assert!(y >= last - 1e-7, "h={h} x={x}");
                assert!(y <= 1.0 + 1e-6);
                last = y;
            }
            assert!(close(shoulder(h, h), 1.0, 1e-5));
        }
    }

    #[test]
    fn shoulder_vanishes_as_headroom_disappears() {
        let h = 1.0 + EXTENDED_EPSILON;
        for i in 0..=100 {
            let x = i as f32 / 100.0;
            assert!(close(shoulder(x, h), x, EXTENDED_EPSILON));
        }
    }

    #[test]
    fn neutral_highlights_keep_texture_instead_of_going_flat() {
        let codes = |mode| {
            (0..=30)
                .map(|i| {
                    let v = 1.0 + 3.0 * i as f32 / 30.0;
                    encode_pixel(tone_map([v; 3], on(4.0), mode))[0]
                })
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(codes(Highlights::Clip).len(), 1);
        assert!(codes(Highlights::Shoulder).len() >= 10);
    }

    #[test]
    fn colored_highlights_keep_their_channel_ratios() {
        let out = tone_map([4.0, 2.0, 1.0], on(4.0), Highlights::Shoulder);
        assert!(close(out[0], 1.0, 1e-6));
        assert!(close(out[1] / out[0], 0.5, 1e-6));
        assert!(close(out[2] / out[0], 0.25, 1e-6));
        // Clip mode projects along the same ray.
        assert_eq!(
            tone_map([4.0, 2.0, 1.0], on(4.0), Highlights::Clip),
            [1.0, 0.5, 0.25]
        );
    }

    fn image(width: u32, height: u32, value: impl Fn(u32, u32) -> f32) -> Vec<[f32; 4]> {
        (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .map(|(x, y)| {
                let v = value(x, y);
                [v, v, v, 1.0]
            })
            .collect()
    }

    fn sdr(code: u8, s: f32) -> f32 {
        code_to_linear(code) * s
    }

    #[test]
    fn sdr_code_values_are_recognized() {
        for s in [1.0, 1.5, 2.4, 3.0, 6.0] {
            for code in 0..=255u8 {
                assert!(
                    on_code_grid([sdr(code, s), sdr(255 - code, s), 0.0], s),
                    "S={s} code={code}"
                );
            }
            // Halfway between two codes, above SDR white, or negative: not SDR.
            let between = srgb_decode(127.5 / 255.0) * s;
            assert!(!on_code_grid([between, 0.0, 0.0], s));
            assert!(!on_code_grid([1.5 * s, 0.0, 0.0], s));
            assert!(!on_code_grid([-0.01 * s, 0.0, 0.0], s));
            // FP16 conversion residue around zero does not count against it.
            assert!(on_code_grid([s, -0.000_005 * s, 0.000_12 * s], s));
            // Nor does cross-talk beside a bright channel: pixels Edge
            // rendered on a green gradient (gate session 2026-09-27, S = 3.5),
            // whose red is 0.27 and 0.37 code off the grid.
            assert!(on_code_grid([0.044_78 * s, 0.999_44 * s, 0.000_03 * s], s));
            assert!(on_code_grid([0.008_42 * s, 1.0 * s, 0.000_04 * s], s));
            // Without a bright channel beside it, the same offset does count.
            assert!(!on_code_grid([0.044_78 * s, 0.044_78 * s, 0.044_78 * s], s));
        }
        // FP16 storage of an SDR code keeps it on the grid.
        for code in 0..=255u8 {
            let v = half::f16::from_f32(sdr(code, 3.0)).to_f32();
            assert!(on_code_grid([v; 3], 3.0), "code={code}");
        }
    }

    /// SDR white right next to HDR content stays exactly white: no halo.
    #[test]
    fn sdr_content_beside_highlights_is_exact() {
        let s = 2.5;
        let (w, h) = (64, 16);
        let src = image(w, h, |x, _| {
            if x < 16 {
                s * (1.5 + 0.2 * x as f32)
            } else {
                s
            }
        });
        let out = convert(&src, w, h, s, Highlights::Shoulder);
        let at = |x: u32| out[(x * 4) as usize];
        // The first white pixel borders HDR content, so it cannot be told
        // apart; from the second on, it is SDR content.
        for x in 17..w {
            assert_eq!(at(x), 255, "x={x}");
        }
        assert!(at(0) < at(15));
        // A frame with no extended pixel is never touched.
        let flat = image(w, h, |x, _| sdr(x as u8 * 3, s));
        let analysis = FrameAnalysis::new(&flat, w, h, s);
        assert!(!analysis.has_extended());
        assert_eq!(convert(&flat, w, h, s, Highlights::Shoulder)[4 * 10], 30);
    }

    /// Non-SDR content below SDR white goes through the same curve as the
    /// highlights, so an HDR image keeps its tonal order.
    #[test]
    fn the_shoulder_is_one_curve_for_the_frame() {
        let (w, h) = (256, 8);
        // Off-grid values, as HDR or video content has.
        let src = image(w, h, |x, _| match x {
            0..32 => 0.95 + 0.0011 * x as f32,
            200..232 => 4.0,
            _ => 0.2,
        });
        let analysis = FrameAnalysis::new(&src, w, h, 1.0);
        assert!(analysis.shoulder_applies(16, 4));
        let out = convert(&src, w, h, 1.0, Highlights::Shoulder);
        assert!(out[16 * 4] < out[216 * 4]);
        assert_eq!(out[216 * 4], 255);
        // Values below the knee are untouched even in HDR content.
        let knee_code = quantize(srgb_encode(0.2));
        assert_eq!(out[100 * 4], knee_code);
    }

    #[test]
    fn chance_matches_inside_hdr_content_are_rare() {
        // Pseudo-random off-grid content: how often does the nine-pixel rule
        // mistake it for SDR content?
        let (w, h) = (256, 256);
        let mut state = 0x9e37_79b9_u32;
        let src: Vec<[f32; 4]> = (0..w * h)
            .map(|_| {
                let mut next = || {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    state as f32 / u32::MAX as f32
                };
                [next() * 3.0, next() * 3.0, next() * 3.0, 1.0]
            })
            .collect();
        let analysis = FrameAnalysis::new(&src, w, h, 3.0);
        let mistaken = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| analysis.sdr_content(x, y))
            .count();
        assert!(
            mistaken * 10_000 < (w * h) as usize,
            "{mistaken} of {}",
            w * h
        );
    }

    #[test]
    fn highlight_mode_names_round_trip() {
        for mode in [Highlights::Shoulder, Highlights::Clip] {
            assert_eq!(Highlights::parse(mode.name()).unwrap(), mode);
        }
        assert!(Highlights::parse("reinhard").is_err());
    }
}
