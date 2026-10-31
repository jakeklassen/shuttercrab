//! A recording sent live (issue #66): its coded frames as the encoder
//! makes them. shuttercrab-stream sends them over the network.
//!
//! No container: each frame goes as soon as it is encoded, in the forms a
//! decoder takes on their own. H.264 and HEVC are Annex B, and AV1 a
//! sequence of OBUs, with the parameter sets before each key frame. AAC has
//! an ADTS header; Opus is its packets; PCM is the samples.

/// Playing a live recording as it arrives.
#[cfg(windows)]
pub use crate::sys::imp::live_player::LivePlayer;

/// Whether this PC can encode pictures as a codec, live.
#[cfg(windows)]
pub use crate::sys::imp::record::encodes;

/// What a live recording is, sent before its frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video: VideoCodec,
    /// How the sound track (48 kHz stereo) is sent, if there is one.
    pub sound: Option<SoundCodec>,
}

/// How a live recording's pictures are coded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    /// H.264, 8 bits: every graphics card decodes it, and Windows always
    /// can.
    H264,
    /// HEVC Main 10, 10 bits: dark gradients keep their steps rather than
    /// band, in fewer bits than H.264. Windows decodes it only with the
    /// HEVC Video Extensions installed.
    Hevc,
    /// AV1 Main, 10 bits: as HEVC, in fewer bits again. Encoded only by
    /// recent graphics cards (NVIDIA's RTX 40, AMD's RX 7000, Intel's Arc),
    /// decoded by fairly recent ones, with Windows' AV1 Video Extension.
    Av1,
}

impl VideoCodec {
    /// The codecs best first: each takes fewer bits than the next for the
    /// same picture, and fewer PCs can play it. A stream takes the first
    /// both ends can.
    pub const BEST_FIRST: [Self; 3] = [Self::Av1, Self::Hevc, Self::H264];

    /// Its bit in a set of codecs.
    pub fn bit(self) -> u8 {
        match self {
            Self::H264 => 1,
            Self::Hevc => 2,
            Self::Av1 => 4,
        }
    }
}

/// How a live recording's sound is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundCodec {
    /// AAC, 0.2 Mbit/s; the encoder holds each frame about 50 ms.
    Aac,
    /// The 16-bit samples as recorded, little-endian: 1.5 Mbit/s, sent as
    /// soon as they are mixed.
    Pcm,
    /// Opus, 0.13 Mbit/s, 10 ms a packet: about 13 ms later than PCM,
    /// sounding the same.
    Opus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Track {
    Video,
    Sound,
}

/// One coded frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveFrame {
    pub track: Track,
    /// When it is shown, in 100 ns ticks on the recording's timeline.
    pub time: i64,
    pub duration: i64,
    /// A picture that decodes on its own.
    pub key: bool,
    pub data: Vec<u8>,
}

/// An AAC frame with an ADTS header: AAC LC, 48 kHz, stereo, as the
/// recorder encodes it.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows recorder streams yet")
)]
pub(crate) fn adts(frame: &[u8]) -> Vec<u8> {
    let len = frame.len() + 7;
    // Profile LC (1), frequency index 3 (48 kHz), two channels.
    let mut out = vec![
        0xFF,
        0xF1,
        (1 << 6) | (3 << 2),
        (2 << 6) | ((len >> 11) & 3) as u8,
        (len >> 3) as u8,
        ((len & 7) << 5) as u8 | 0x1F,
        0xFC,
    ];
    out.extend_from_slice(frame);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_adts_header_gives_the_frame_its_length() {
        let framed = adts(&[0xAB; 371]);
        assert_eq!(framed.len(), 378);
        // The 13-bit length across bytes 3 to 5.
        let len = ((framed[3] as usize & 3) << 11)
            | ((framed[4] as usize) << 3)
            | (framed[5] as usize >> 5);
        assert_eq!(len, 378);
        assert_eq!(&framed[..3], &[0xFF, 0xF1, 0x4C]);
        assert_eq!(framed[3] >> 6, 2, "two channels");
    }
}
