//! Live streaming (issue #66): a recording sent to a viewer as it is made,
//! and the window that plays it as it arrives.
//!
//! The streaming app opens a room on the meeting point and shares its
//! link ([`Room`]), or waits for a viewer on this PC; either way it gets a
//! [`Viewer`], and a [`Stream`] records into it. The viewer opens the link
//! ([`Input`]) and [`watch`](fn@watch)es it in a window of its own.
//!
//! Online, the pictures and the sound go on separate QUIC streams, each
//! starting with a byte that says which it is, so a large picture cannot
//! hold up the sound. The viewer first says which codecs it can play, a
//! bit each, and the stream takes the best one both ends can.

mod quality;
#[cfg_attr(not(windows), allow(dead_code, reason = "only Windows streams yet"))]
mod wire;

#[cfg(windows)]
mod host;
#[cfg(windows)]
mod watch;

pub use quality::Quality;
pub use shuttercrab_connect::{Link, MEET, NOT_THE_HOST_KEY};

#[cfg(windows)]
pub use host::{Room, Stats, Stream, Viewer, wait_locally};
#[cfg(windows)]
pub use watch::{Input, WatchOptions, tell, watch};

use shuttercrab_capture::live::VideoCodec;

/// The best codec from `best` down that this PC encodes, by `encodes`, and
/// the viewer `plays` (a bit each); H.264 if none.
pub fn playable(best: VideoCodec, plays: u8, encodes: impl Fn(VideoCodec) -> bool) -> VideoCodec {
    VideoCodec::BEST_FIRST
        .into_iter()
        .skip_while(|&codec| codec != best)
        .find(|&codec| plays & codec.bit() != 0 && encodes(codec))
        .unwrap_or(VideoCodec::H264)
}

/// A codec as people read it.
pub fn describe(video: VideoCodec) -> &'static str {
    match video {
        VideoCodec::H264 => "H.264, 8 bits",
        VideoCodec::Hevc => "HEVC, 10 bits",
        VideoCodec::Av1 => "AV1, 10 bits",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stream_takes_the_best_codec_both_ends_can() {
        let all = |_| true;
        let both = VideoCodec::H264.bit() | VideoCodec::Hevc.bit() | VideoCodec::Av1.bit();
        assert_eq!(playable(VideoCodec::Av1, both, all), VideoCodec::Av1);
        // The viewer has no AV1 decoder.
        let no_av1 = both & !VideoCodec::Av1.bit();
        assert_eq!(playable(VideoCodec::Av1, no_av1, all), VideoCodec::Hevc);
        // This PC has no AV1 encoder.
        let no_av1_here = |c| c != VideoCodec::Av1;
        assert_eq!(
            playable(VideoCodec::Av1, both, no_av1_here),
            VideoCodec::Hevc
        );
        // Asked to start lower.
        assert_eq!(playable(VideoCodec::Hevc, both, all), VideoCodec::Hevc);
        // Nothing in common: H.264, which every PC plays.
        assert_eq!(playable(VideoCodec::Av1, 0, all), VideoCodec::H264);
    }
}
