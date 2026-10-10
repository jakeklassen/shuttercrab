//! Steady HDR exposure across a recording's frames.

use super::TICKS_PER_SECOND;
use crate::color::{Analysis, HdrRegion, SCREENSHOT_ANCHOR, anchor_regions};

/// Keeps HDR exposure steady across frames: the 90th-percentile anchor of
/// each frame, eased towards over half a second, so video neither pumps nor
/// flickers.
pub(crate) struct Exposure {
    pub(crate) value: Option<f32>,
    pub(crate) last: i64,
}

impl Exposure {
    const SETTLE_SECONDS: f32 = 0.5;

    pub(crate) fn apply(&mut self, analysis: &mut Analysis, time: i64) {
        if analysis.regions.is_empty() {
            return;
        }
        anchor_regions(
            &mut analysis.regions,
            &analysis.tiles,
            analysis.tiles_x,
            SCREENSHOT_ANCHOR,
        );
        let target = analysis
            .regions
            .iter()
            .map(|r| r.peak)
            .fold(1.0f32, f32::max);
        let value = match self.value {
            None => target,
            Some(value) => {
                let dt = (time - self.last).max(0) as f32 / TICKS_PER_SECOND as f32;
                let ease = 1.0 - (-dt / Self::SETTLE_SECONDS).exp();
                value + (target - value) * ease
            }
        };
        self.value = Some(value);
        self.last = time;
        set_peaks(&mut analysis.regions, value);
    }
}

fn set_peaks(regions: &mut [HdrRegion], peak: f32) {
    for region in regions {
        region.peak = peak.max(1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analysis(peaks: &[f32]) -> Analysis {
        use crate::color::{TILE, TileStats, find_regions};
        let tiles: Vec<TileStats> = peaks
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
        let regions = find_regions(&tiles, peaks.len() as u32, 1);
        Analysis {
            tiles,
            tiles_x: peaks.len() as u32,
            regions,
            frame_peak: peaks.iter().copied().fold(0.0, f32::max),
        }
    }

    #[test]
    fn exposure_eases_towards_a_new_scene_instead_of_jumping() {
        let mut exposure = Exposure {
            value: None,
            last: 0,
        };
        let mut first = analysis(&[4.0; 10]);
        exposure.apply(&mut first, 0);
        assert_eq!(first.regions[0].peak, 4.0);
        // The scene gets much brighter one frame later: the anchor moves only
        // part of the way (1/30 s of a 0.5 s settle).
        let mut next = analysis(&[8.0; 10]);
        exposure.apply(&mut next, TICKS_PER_SECOND / 30);
        let peak = next.regions[0].peak;
        assert!(peak > 4.0 && peak < 4.5, "{peak}");
        // After two seconds it has arrived.
        let mut later = analysis(&[8.0; 10]);
        exposure.apply(&mut later, 2 * TICKS_PER_SECOND);
        assert!((later.regions[0].peak - 8.0).abs() < 0.1);
    }

    #[test]
    fn frames_without_hdr_content_are_left_alone() {
        let mut exposure = Exposure {
            value: None,
            last: 0,
        };
        let mut plain = Analysis {
            tiles: Vec::new(),
            tiles_x: 0,
            regions: Vec::new(),
            frame_peak: 1.0,
        };
        exposure.apply(&mut plain, 0);
        assert!(plain.regions.is_empty());
        assert_eq!(exposure.value, None);
    }
}
