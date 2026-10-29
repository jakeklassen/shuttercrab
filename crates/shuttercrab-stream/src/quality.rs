use serde::{Deserialize, Serialize};

/// A size to stream at, and the bit rate for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality {
    /// Light on both ends: the viewer's graphics card decodes a quarter of
    /// what 4K takes.
    #[default]
    P1080,
    P1440,
    P2160,
}

impl Quality {
    /// The choices, in order: keys 1, 2 and 3.
    pub const ALL: [Self; 3] = [Self::P1080, Self::P1440, Self::P2160];

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "1080p" => Some(Self::P1080),
            "1440p" => Some(Self::P1440),
            "4k" | "4K" | "2160p" => Some(Self::P2160),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::P1080 => "1080p",
            Self::P1440 => "1440p",
            Self::P2160 => "4K",
        }
    }

    /// Bits a second: under what Cloudflare's relay passes for one app
    /// (about 50 Mbit/s), with room for the peaks of busy pictures.
    pub fn bitrate(self) -> u32 {
        match self {
            Self::P1080 => 10_000_000,
            Self::P1440 => 18_000_000,
            Self::P2160 => 35_000_000,
        }
    }

    /// The size `w`×`h` is sent at: this height, the same shape, even, and
    /// never larger than it is.
    pub fn size(self, w: u32, h: u32) -> (u32, u32) {
        let target = match self {
            Self::P1080 => 1080,
            Self::P1440 => 1440,
            Self::P2160 => 2160,
        };
        if h <= target {
            return (w & !1, h & !1);
        }
        let scaled = (u64::from(w) * u64::from(target) / u64::from(h)) as u32;
        (scaled & !1, target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quality_scales_down_to_its_height_and_never_up() {
        assert_eq!(Quality::P1080.size(3840, 2160), (1920, 1080));
        assert_eq!(Quality::P1440.size(3440, 1440), (3440, 1440));
        assert_eq!(Quality::P2160.size(1921, 1081), (1920, 1080));
        // An odd width at the new height is made even.
        assert_eq!(Quality::P1080.size(2001, 2160), (1000, 1080));
        assert_eq!(Quality::parse("4k"), Some(Quality::P2160));
        assert_eq!(Quality::default(), Quality::P1080);
    }
}
