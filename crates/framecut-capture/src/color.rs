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
pub const TRANSFORM_VERSION: &str = "sdr-exact-hdr-regions-v4";

/// Side of the square tiles the frame is analysed in, in pixels.
pub const TILE: u32 = 16;
/// Normalized values up to `1 + EXTENDED_EPSILON` are SDR white. This absorbs
/// FP16 rounding of `S × 1.0` (at most 2⁻¹¹ relative) with margin, so SDR
/// white never registers as an HDR highlight.
pub const EXTENDED_EPSILON: f32 = 1.0 / 256.0;
/// BT.709 / sRGB relative luminance weights.
pub const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];
/// Largest finite FP16 value; scRGB input is clamped to it.
const FP16_MAX: f32 = 65504.0;

/// How values above SDR white are brought into range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Highlights {
    /// Default. Content outside HDR regions is reproduced as captured;
    /// HDR regions go through `hdr_curve`.
    Tonemap,
    /// Diagnostic. Every pixel is projected onto the SDR cube along its RGB
    /// ray. This is the prior spike's behaviour: neutral highlights go flat white.
    Clip,
}

impl Highlights {
    pub fn name(self) -> &'static str {
        match self {
            Highlights::Tonemap => "tonemap",
            Highlights::Clip => "clip",
        }
    }

    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "tonemap" => Ok(Highlights::Tonemap),
            "clip" => Ok(Highlights::Clip),
            _ => bail!("unknown highlight mode {name:?}; expected tonemap or clip"),
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
/// curve's knee, so whether it is SDR content never matters.
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

/// Where HDR content is: its rectangles and each one's peak. Found from
/// per-tile statistics, which the GPU computes and the CPU turns into
/// rectangles with `find_regions`.
///
/// A tile qualifies when it holds a pixel above SDR white, or when at least
/// a quarter of its pixels are not SDR content. Qualifying tiles are grown by
/// one tile to join the pieces of one image or video; an area that holds no
/// pixel above SDR white is left alone (SDR video, compositor effects). The
/// rectangle is the bounding box of the area's non-SDR pixels, shrunk by the
/// one pixel: the ring of SDR pixels the 3×3 rule marks around any non-SDR
/// content.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileStats {
    /// Peak of the normalized pixels.
    pub peak: f32,
    /// Pixels that are not SDR content.
    pub non_sdr: u32,
    /// Pixels above SDR white.
    pub extended: u32,
    /// Bounding box of the non-SDR pixels, in frame pixels. Empty when
    /// `min_x > max_x`.
    pub min_x: u32,
    pub min_y: u32,
    pub max_x: u32,
    pub max_y: u32,
}

impl Default for TileStats {
    fn default() -> Self {
        Self {
            peak: 0.0,
            non_sdr: 0,
            extended: 0,
            min_x: u32::MAX,
            min_y: u32::MAX,
            max_x: 0,
            max_y: 0,
        }
    }
}

/// A rectangle of HDR content, inclusive, in frame pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrRegion {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    /// Peak of the content inside, over SDR white.
    pub peak: f32,
}

impl HdrRegion {
    pub fn contains(&self, x: u32, y: u32) -> bool {
        (self.x0..=self.x1).contains(&x) && (self.y0..=self.y1).contains(&y)
    }
}

/// A tile qualifies when this share of it (1/4) is not SDR content.
pub const REGION_SHARE: u32 = 4;
/// The shader holds at most this many regions; beyond that they are merged.
pub const MAX_REGIONS: usize = 32;

pub fn find_regions(stats: &[TileStats], tiles_x: u32, tiles_y: u32) -> Vec<HdrRegion> {
    let (tw, th) = (tiles_x as i64, tiles_y as i64);
    let index = |x: i64, y: i64| (y * tw + x) as usize;
    let qualifies: Vec<bool> = stats
        .iter()
        .map(|s| s.extended > 0 || s.non_sdr * REGION_SHARE >= TILE * TILE)
        .collect();
    let grown: Vec<bool> = (0..th)
        .flat_map(|y| (0..tw).map(move |x| (x, y)))
        .map(|(x, y)| {
            (-1..=1).any(|dy| {
                (-1..=1).any(|dx| {
                    let (nx, ny) = (x + dx, y + dy);
                    (0..tw).contains(&nx) && (0..th).contains(&ny) && qualifies[index(nx, ny)]
                })
            })
        })
        .collect();
    let mut seen = vec![false; stats.len()];
    let mut regions = Vec::new();
    for start in 0..stats.len() {
        if !grown[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        let mut stack = vec![start];
        let (mut extended, mut peak) = (false, 0.0f32);
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        while let Some(i) = stack.pop() {
            let s = &stats[i];
            if qualifies[i] {
                extended |= s.extended > 0;
                peak = peak.max(s.peak);
            }
            // The box takes every tile of the area, so the one-pixel border is
            // trimmed evenly whichever tiles it falls in.
            if s.min_x <= s.max_x {
                (x0, y0, x1, y1) = (
                    x0.min(s.min_x),
                    y0.min(s.min_y),
                    x1.max(s.max_x),
                    y1.max(s.max_y),
                );
            }
            let (x, y) = (i as i64 % tw, i as i64 / tw);
            for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                let (nx, ny) = (x + dx, y + dy);
                if (0..tw).contains(&nx) && (0..th).contains(&ny) {
                    let j = index(nx, ny);
                    if grown[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        if !extended || x0 > x1 {
            continue;
        }
        if x1 - x0 >= 2 && y1 - y0 >= 2 {
            (x0, y0, x1, y1) = (x0 + 1, y0 + 1, x1 - 1, y1 - 1);
        }
        regions.push(HdrRegion {
            x0,
            y0,
            x1,
            y1,
            peak,
        });
    }
    if regions.len() > MAX_REGIONS {
        let merged = regions.iter().fold(regions[0], |a, r| HdrRegion {
            x0: a.x0.min(r.x0),
            y0: a.y0.min(r.y0),
            x1: a.x1.max(r.x1),
            y1: a.y1.max(r.y1),
            peak: a.peak.max(r.peak),
        });
        regions = vec![merged];
    }
    regions
}

/// Output level up to which HDR content stays linear after its gain.
pub const TONE_KNEE: f32 = 0.45;

/// What sets the exposure of an HDR region: the brightness the curve maps
/// to white (PRD §9.6; COLOR_PIPELINE.md, "Peak-driven gain").
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Anchor {
    /// The brightest pixel in the region: nothing clips, but one spark
    /// dims the whole region, and a different crop can change it.
    #[default]
    RegionPeak,
    /// This quantile (0 to 1) of the region's tile peaks: rare sparks are
    /// ignored (and clip to white), so the exposure follows most of the
    /// content.
    Percentile(f32),
    /// A fixed brightness over SDR white, such as the display's peak: the
    /// same exposure whatever the content or the crop.
    Fixed(f32),
}

/// The anchor for screenshots: the 90th percentile of a region's tile
/// peaks. Chosen by the owner on a real HDR video frame (2026-09-28): it
/// came closest to the video player's own HDR-off rendering (mean ΔE00
/// 0.80, against 2.57 for the region peak), and the HDR test image's eight
/// grey steps stay distinct (docs/COLOR_PIPELINE.md).
pub const SCREENSHOT_ANCHOR: Anchor = Anchor::Percentile(0.9);

/// Set each region's `peak` to the brightness `anchor` maps to white.
/// `stats` and `tiles_x` are what [`find_regions`] used.
pub fn anchor_regions(
    regions: &mut [HdrRegion],
    stats: &[TileStats],
    tiles_x: u32,
    anchor: Anchor,
) {
    for region in regions {
        region.peak = match anchor {
            Anchor::RegionPeak => region.peak,
            Anchor::Fixed(peak) => peak,
            Anchor::Percentile(q) => {
                // Tiles of the region that hold HDR content (as in
                // find_regions), by their centre.
                let mut peaks: Vec<f32> = stats
                    .iter()
                    .enumerate()
                    .filter(|(i, s)| {
                        let (tx, ty) = (*i as u32 % tiles_x, *i as u32 / tiles_x);
                        let (cx, cy) = (tx * TILE + TILE / 2, ty * TILE + TILE / 2);
                        region.contains(cx, cy)
                            && (s.extended > 0 || s.non_sdr * REGION_SHARE >= TILE * TILE)
                    })
                    .map(|(_, s)| s.peak)
                    .collect();
                if peaks.is_empty() {
                    region.peak
                } else {
                    peaks.sort_by(f32::total_cmp);
                    let at = ((peaks.len() - 1) as f32 * q.clamp(0.0, 1.0)).round() as usize;
                    peaks[at].min(region.peak)
                }
            }
        };
    }
}

/// The curve for HDR content with peak `p` (over SDR white): dim by
/// `g = 1/√p`, keep linear up to `TONE_KNEE`, then roll off with an
/// extended-Reinhard shoulder that reaches 1 exactly at the peak. Fitted to
/// Edge's own HDR-off rendering of the HDR test image (docs/COLOR_PIPELINE.md);
/// the project owner chose it over lighter curves.
pub fn hdr_curve(m: f32, p: f32) -> f32 {
    let p = p.max(1.0);
    let g = 1.0 / p.sqrt();
    let top = p.sqrt().max(TONE_KNEE + 1e-3);
    let x = g * m;
    if x <= TONE_KNEE {
        return x;
    }
    let a = (x.min(top) - TONE_KNEE) / (1.0 - TONE_KNEE);
    let big = (top - TONE_KNEE) / (1.0 - TONE_KNEE);
    TONE_KNEE + (1.0 - TONE_KNEE) * a * (1.0 + a / (big * big)) / (1.0 + a)
}

/// What `tone_map` needs to know about a pixel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PixelContext {
    /// The peak of the HDR region the pixel lies in, if it lies in one.
    pub region_peak: Option<f32>,
}

impl PixelContext {
    /// Outside any HDR region: left as it is.
    pub const SDR: Self = Self { region_peak: None };
}

/// Step 3 and 4: bring a normalized pixel into the SDR cube. Returns linear
/// sRGB in `[0, 1]`. Inside an HDR region the curve applies to the pixel's
/// peak channel, scaling all three channels alike; everything else is left as
/// it is, and anything still outside the cube is projected onto it along its
/// RGB ray.
pub fn tone_map(n: [f32; 3], at: PixelContext, mode: Highlights) -> [f32; 3] {
    let mut n = n;
    if let (Highlights::Tonemap, Some(p)) = (mode, at.region_peak) {
        let m = peak(n);
        if m > 0.0 {
            let scale = hdr_curve(m, p) / m;
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

/// The per-frame analysis the shader does before converting.
pub struct FrameAnalysis {
    pub width: u32,
    pub height: u32,
    pub frame_peak: f32,
    pub on_grid: Vec<bool>,
    pub tiles: Vec<TileStats>,
    pub regions: Vec<HdrRegion>,
}

impl FrameAnalysis {
    pub fn new(scrgb: &[[f32; 4]], width: u32, height: u32, white_scale: f32) -> Self {
        let on_grid: Vec<bool> = scrgb
            .iter()
            .map(|p| on_code_grid([p[0], p[1], p[2]], white_scale))
            .collect();
        let (tiles_x, tiles_y) = (width.div_ceil(TILE), height.div_ceil(TILE));
        let mut analysis = Self {
            width,
            height,
            frame_peak: 0.0,
            on_grid,
            tiles: vec![TileStats::default(); (tiles_x * tiles_y) as usize],
            regions: Vec::new(),
        };
        for y in 0..height {
            for x in 0..width {
                let p = scrgb[(y * width + x) as usize];
                let m = peak(normalize([p[0], p[1], p[2]], white_scale));
                let sdr = analysis.sdr_content(x, y);
                let t = &mut analysis.tiles[((y / TILE) * tiles_x + x / TILE) as usize];
                t.peak = t.peak.max(m);
                t.extended += u32::from(m > 1.0 + EXTENDED_EPSILON);
                if !sdr {
                    t.non_sdr += 1;
                    (t.min_x, t.min_y, t.max_x, t.max_y) = (
                        t.min_x.min(x),
                        t.min_y.min(y),
                        t.max_x.max(x),
                        t.max_y.max(y),
                    );
                }
            }
        }
        analysis.frame_peak = analysis.tiles.iter().fold(0.0, |a, t| a.max(t.peak));
        analysis.regions = find_regions(&analysis.tiles, tiles_x, tiles_y);
        analysis
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

    pub fn region_at(&self, x: u32, y: u32) -> Option<&HdrRegion> {
        self.regions.iter().find(|r| r.contains(x, y))
    }

    pub fn at(&self, x: u32, y: u32) -> PixelContext {
        PixelContext {
            region_peak: self.region_at(x, y).map(|r| r.peak),
        }
    }

    /// Whether any pixel is brighter than SDR white.
    pub fn has_extended(&self) -> bool {
        self.frame_peak > 1.0 + EXTENDED_EPSILON
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

    /// A pixel in an HDR region whose peak is `peak`.
    fn region(peak: f32) -> PixelContext {
        PixelContext {
            region_peak: Some(peak),
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
                    let out = encode_pixel(tone_map(n, PixelContext::SDR, Highlights::Tonemap));
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
                Highlights::Tonemap,
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
            encode_pixel(tone_map(n, PixelContext::SDR, Highlights::Tonemap)),
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
            encode_pixel(tone_map(n, region(n[0]), Highlights::Tonemap)),
            [255, 0, 0]
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

    /// Measured on the HDR test image in Edge (gate session 2026-09-27): the
    /// values Edge placed its 100…500-nit steps at with HDR on at SDR white
    /// 120 nits (frame peak 3.79), and the codes Edge itself rendered them
    /// at with HDR off. The curve was fitted there, so it must stay close.
    #[test]
    fn hdr_curve_tracks_edges_own_hdr_off_rendering() {
        let v = [
            0.493_164, 0.738_281, 0.998_047, 1.220_703, 1.444_01, 1.647_135, 1.843_75, 2.222_656,
        ];
        let edge = [136u8, 164, 187, 201, 211, 218, 223, 232];
        for (v, want) in v.iter().zip(edge) {
            let got = quantize(srgb_encode(hdr_curve(*v, 3.79)));
            assert!(got.abs_diff(want) <= 4, "v={v}: got {got}, Edge {want}");
        }
    }

    #[test]
    fn hdr_curve_is_monotonic_and_reaches_white_at_the_peak() {
        for p in [1.01f32, 1.06, 1.62, 3.79, 10.0, 100.0] {
            let mut last = 0.0;
            for i in 0..=2000 {
                let m = p * i as f32 / 2000.0;
                let y = hdr_curve(m, p);
                assert!(y >= last - 1e-6 && y <= 1.0 + 1e-6, "p={p} m={m}");
                last = y;
            }
            assert!(close(hdr_curve(p, p), 1.0, 1e-5), "p={p}");
        }
        // Distinct highlights stay distinct: the eight steps are eight codes.
        let steps: std::collections::BTreeSet<u8> =
            [1.22f32, 1.44, 1.65, 1.84, 2.22, 2.6, 3.0, 3.4]
                .iter()
                .map(|v| quantize(srgb_encode(hdr_curve(*v, 3.79))))
                .collect();
        assert_eq!(steps.len(), 8);
    }

    #[test]
    fn colored_highlights_keep_their_channel_ratios() {
        let out = tone_map([4.0, 2.0, 1.0], region(4.0), Highlights::Tonemap);
        assert!(close(out[0], 1.0, 1e-5));
        assert!(close(out[1] / out[0], 0.5, 1e-5));
        assert!(close(out[2] / out[0], 0.25, 1e-5));
        assert_eq!(
            tone_map([4.0, 2.0, 1.0], region(4.0), Highlights::Clip),
            [1.0, 0.5, 0.25]
        );
    }

    /// An HDR patch in a page of SDR codes, with a flat part that lands on
    /// the code grid (as Edge's 203-nit step does).
    fn page_with_hdr_patch(s: f32) -> (u32, u32, Vec<[f32; 4]>) {
        let (w, h) = (256u32, 128u32);
        let pixels = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let v = if (112..128).contains(&x) && (52..68).contains(&y) {
                    s
                } else if (100..160).contains(&x) && (40..80).contains(&y) {
                    s * (0.2 + 0.041 * (x - 100) as f32 + 0.0013 * (y - 40) as f32)
                } else {
                    code_to_linear(((x * 3 + y) % 256) as u8) * s
                };
                [v, v, v, 1.0]
            })
            .collect();
        (w, h, pixels)
    }

    #[test]
    fn hdr_content_is_found_and_sdr_around_it_is_exact() {
        let s = 2.0;
        let (w, h, src) = page_with_hdr_patch(s);
        let analysis = FrameAnalysis::new(&src, w, h, s);
        assert_eq!(analysis.regions.len(), 1, "{:?}", analysis.regions);
        let r = analysis.regions[0];
        // Exactly the patch: its non-SDR box includes the 3×3 rule's ring,
        // which the one-pixel trim removes.
        assert_eq!((r.x0, r.y0, r.x1, r.y1), (100, 40, 159, 79), "{r:?}");
        assert!(close(r.peak, 0.2 + 0.041 * 59.0 + 0.0013 * 39.0, 1e-3));
        let out = convert(&src, w, h, s, Highlights::Tonemap);
        for y in 0..h {
            for x in 0..w {
                if !(99..=160).contains(&x) || !(39..=80).contains(&y) {
                    let want = ((x * 3 + y) % 256) as u8;
                    assert_eq!(out[((y * w + x) * 4) as usize], want, "({x},{y})");
                }
            }
        }
        // The flat part at exactly SDR white is dimmed like the rest of the
        // patch, so it is not whiter than brighter content beside it.
        let flat = out[((60 * w + 120) * 4) as usize];
        let brighter = out[((60 * w + 150) * 4) as usize];
        assert!(flat < brighter, "flat {flat}, brighter {brighter}");
    }

    #[test]
    fn content_without_highlights_is_never_tone_mapped() {
        // Off-grid content that never exceeds SDR white: SDR video, blending.
        let s = 3.0;
        let (w, h) = (128, 64);
        let src = image(w, h, |x, y| {
            s * (0.1 + 0.0071 * x as f32 + 0.0013 * y as f32).min(1.0)
        });
        let analysis = FrameAnalysis::new(&src, w, h, s);
        assert!(analysis.regions.is_empty());
        let out = convert(&src, w, h, s, Highlights::Tonemap);
        for (i, px) in out.as_chunks::<4>().0.iter().enumerate() {
            let v = src[i][0] / s;
            assert_eq!(px[0], quantize(srgb_encode(v)));
        }
    }

    #[test]
    fn separate_hdr_areas_get_their_own_peak() {
        let s = 1.0;
        let (w, h) = (320, 64);
        let src = image(w, h, |x, _| match x {
            20..60 => 1.5 + 0.001 * x as f32,
            240..300 => 4.0 + 0.001 * x as f32,
            _ => code_to_linear(255),
        });
        let analysis = FrameAnalysis::new(&src, w, h, s);
        assert_eq!(analysis.regions.len(), 2, "{:?}", analysis.regions);
        assert!(analysis.regions[0].peak < 1.6 && analysis.regions[1].peak > 4.0);
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
        for mode in [Highlights::Tonemap, Highlights::Clip] {
            assert_eq!(Highlights::parse(mode.name()).unwrap(), mode);
        }
        assert!(Highlights::parse("reinhard").is_err());
    }
}

#[cfg(test)]
mod anchor_tests {
    use super::*;

    /// A 4×1 row of HDR tiles whose peaks are `peaks`, in one region.
    fn row(peaks: [f32; 4]) -> (Vec<TileStats>, Vec<HdrRegion>) {
        let stats: Vec<TileStats> = peaks
            .iter()
            .enumerate()
            .map(|(i, &peak)| TileStats {
                peak,
                non_sdr: TILE * TILE,
                extended: 1,
                min_x: i as u32 * TILE,
                min_y: 0,
                max_x: i as u32 * TILE + TILE - 1,
                max_y: TILE - 1,
            })
            .collect();
        let regions = find_regions(&stats, 4, 1);
        (stats, regions)
    }

    #[test]
    fn the_region_peak_follows_its_brightest_tile() {
        let (stats, mut regions) = row([2.0, 2.2, 2.4, 9.0]);
        assert_eq!(regions.len(), 1);
        anchor_regions(&mut regions, &stats, 4, Anchor::RegionPeak);
        assert_eq!(regions[0].peak, 9.0);
    }

    #[test]
    fn a_percentile_ignores_one_spark() {
        let (stats, mut regions) = row([2.0, 2.2, 2.4, 9.0]);
        anchor_regions(&mut regions, &stats, 4, Anchor::Percentile(0.5));
        // Median of four by rounding: the third value.
        assert_eq!(regions[0].peak, 2.4);
        // Never above the real peak.
        let (stats, mut regions) = row([2.0, 2.2, 2.4, 9.0]);
        anchor_regions(&mut regions, &stats, 4, Anchor::Percentile(1.0));
        assert_eq!(regions[0].peak, 9.0);
    }

    #[test]
    fn a_fixed_anchor_ignores_the_content() {
        for peaks in [[2.0, 2.2, 2.4, 9.0], [1.5, 1.5, 1.5, 1.5]] {
            let (stats, mut regions) = row(peaks);
            anchor_regions(&mut regions, &stats, 4, Anchor::Fixed(4.0));
            assert_eq!(regions[0].peak, 4.0);
        }
    }
}
