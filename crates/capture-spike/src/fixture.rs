//! The HDR test image for the mixed-content scene: a 16-bit PNG, BT.2020
//! primaries, PQ transfer (cICP 9/16/0/1), with known absolute luminances.
//!
//! Bands, top to bottom, 128 px each, 1024 px wide:
//! 1. a neutral ramp from 0 to 1000 nits, linear in PQ;
//! 2. neutral steps at 100, 150, 203, 250, 300, 350, 400 and 500 nits;
//! 3. four 32 px strips of neutral texture, swinging over 150–250,
//!    250–350, 350–450 and 450–800 nits. Windows and the browser limit HDR
//!    content to the display's peak, so the strips span what common panels
//!    show; whichever lie above SDR white hold detail that clipping flattens;
//! 4. colored highlights at 400 nits (BT.709 primaries and secondaries,
//!    orange, and white).

use crate::png_io;
use anyhow::Result;
use std::path::Path;

pub const WIDTH: u32 = 1024;
pub const HEIGHT: u32 = 512;
const BAND: u32 = 128;

/// SMPTE ST 2084 inverse EOTF: absolute nits → PQ signal in [0, 1].
pub fn pq_encode(nits: f64) -> f64 {
    const M1: f64 = 2610.0 / 16384.0;
    const M2: f64 = 2523.0 / 4096.0 * 128.0;
    const C1: f64 = 3424.0 / 4096.0;
    const C2: f64 = 2413.0 / 4096.0 * 32.0;
    const C3: f64 = 2392.0 / 4096.0 * 32.0;
    let y = (nits / 10_000.0).clamp(0.0, 1.0).powf(M1);
    ((C1 + C2 * y) / (1.0 + C3 * y)).powf(M2)
}

/// Linear BT.709 RGB → linear BT.2020 RGB (ITU-R BT.2087).
pub fn bt709_to_bt2020(c: [f64; 3]) -> [f64; 3] {
    const M: [[f64; 3]; 3] = [
        [0.6274, 0.3293, 0.0433],
        [0.0691, 0.9195, 0.0114],
        [0.0164, 0.0880, 0.8956],
    ];
    M.map(|row| row[0] * c[0] + row[1] * c[1] + row[2] * c[2])
}

/// The pixel at (x, y) as BT.2020 linear light in nits.
pub fn nits_at(x: u32, y: u32) -> [f64; 3] {
    let neutral = |n: f64| [n; 3];
    match y / BAND {
        0 => {
            // Linear in PQ signal, so find the nits for this signal.
            let target = pq_encode(1000.0) * x as f64 / (WIDTH - 1) as f64;
            let (mut lo, mut hi) = (0.0, 1000.0);
            for _ in 0..60 {
                let mid = (lo + hi) / 2.0;
                if pq_encode(mid) < target {
                    lo = mid
                } else {
                    hi = mid
                }
            }
            neutral(lo)
        }
        1 => {
            const STEPS: [f64; 8] = [100.0, 150.0, 203.0, 250.0, 300.0, 350.0, 400.0, 500.0];
            neutral(STEPS[(x / (WIDTH / 8)) as usize])
        }
        2 => {
            let (fx, fy) = (x as f64, (y % BAND) as f64);
            let t = (fx / 9.0).sin() * (fy / 7.0).sin() * 0.7 + (fx / 31.0 + fy / 23.0).sin() * 0.3;
            const STRIPS: [(f64, f64); 4] = [
                (150.0, 250.0),
                (250.0, 350.0),
                (350.0, 450.0),
                (450.0, 800.0),
            ];
            let (lo, hi) = STRIPS[((y % BAND) / (BAND / 4)) as usize];
            neutral(lo + (hi - lo) * (t + 1.0) / 2.0)
        }
        _ => {
            const COLORS: [[f64; 3]; 8] = [
                [1.0, 0.0, 0.0],
                [1.0, 0.5, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 1.0, 1.0],
                [0.0, 0.0, 1.0],
                [1.0, 0.0, 1.0],
                [1.0, 1.0, 1.0],
            ];
            let c = COLORS[(x / (WIDTH / 8)) as usize];
            bt709_to_bt2020(c.map(|v| v * 400.0))
        }
    }
}

pub fn write_hdr_test_image(path: &Path) -> Result<()> {
    let mut rgb = Vec::with_capacity((WIDTH * HEIGHT * 3) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            rgb.extend(nits_at(x, y).map(|n| (pq_encode(n) * 65535.0).round() as u16));
        }
    }
    png_io::write_pq16(path, WIDTH, HEIGHT, &rgb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pq_matches_reference_values() {
        assert!((pq_encode(10_000.0) - 1.0).abs() < 1e-12);
        assert!(pq_encode(0.0) < 1e-6);
        // BT.2100 / BT.2408 reference points.
        assert!((pq_encode(100.0) - 0.5081).abs() < 1e-4);
        assert!((pq_encode(203.0) - 0.5806).abs() < 1e-4);
        assert!((pq_encode(1000.0) - 0.7518).abs() < 1e-4);
    }

    #[test]
    fn white_stays_white_in_bt2020() {
        let w = bt709_to_bt2020([1.0; 3]);
        assert!(w.iter().all(|v| (v - 1.0).abs() < 1e-3));
    }

    #[test]
    fn bands_hold_their_luminances() {
        assert_eq!(nits_at(0, 200), [100.0; 3]);
        assert_eq!(nits_at(WIDTH - 1, 200), [500.0; 3]);
        assert!(nits_at(0, 0)[0] < 1e-6);
        assert!((nits_at(WIDTH - 1, 0)[0] - 1000.0).abs() < 1e-6);
        let texture: Vec<f64> = (0..WIDTH).map(|x| nits_at(x, 290)[0]).collect();
        let (lo, hi) = texture
            .iter()
            .fold((f64::MAX, 0.0f64), |(a, b), &v| (a.min(v), b.max(v)));
        // Row 290 lies in the 250–350 nit strip.
        assert!(lo >= 250.0 - 1e-9 && hi <= 350.0 + 1e-9 && hi - lo > 60.0);
    }
}
