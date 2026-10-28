//! A recording sent live (issue #66, spike S0): its coded frames as the
//! encoder makes them, and their wire format.
//!
//! No container: each frame goes as soon as it is encoded, in the forms a
//! decoder takes on their own. H.264 and HEVC are Annex B, with the
//! parameter sets before each key frame; AAC has an ADTS header; PCM is the samples.

/// Playing a live recording as it arrives.
#[cfg(windows)]
pub use crate::sys::imp::watch::LivePlayer;

use anyhow::{Result, bail, ensure};
use std::io::{Read, Write};

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
}

/// How a live recording's sound is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundCodec {
    /// AAC, 0.2 Mbit/s; the encoder holds each frame about 50 ms.
    Aac,
    /// The 16-bit samples as recorded, little-endian: 1.5 Mbit/s, sent as
    /// soon as they are mixed.
    Pcm,
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

/// "Shuttercrab live", version 2: the pictures' codec follows the frame rate.
const MAGIC: [u8; 4] = *b"SCL2";

impl LiveFormat {
    pub fn write_to(&self, out: &mut impl Write) -> Result<()> {
        out.write_all(&MAGIC)?;
        for n in [self.width, self.height, self.fps] {
            out.write_all(&n.to_le_bytes())?;
        }
        out.write_all(&[match self.video {
            VideoCodec::H264 => 1,
            VideoCodec::Hevc => 2,
        }])?;
        let sound = match self.sound {
            None => 0,
            Some(SoundCodec::Aac) => 1,
            Some(SoundCodec::Pcm) => 2,
        };
        out.write_all(&[sound])?;
        Ok(())
    }

    pub fn read_from(input: &mut impl Read) -> Result<Self> {
        let mut magic = [0; 4];
        input.read_exact(&mut magic)?;
        ensure!(magic == MAGIC, "not a Shuttercrab live stream");
        let mut n = || -> Result<u32> {
            let mut bytes = [0; 4];
            input.read_exact(&mut bytes)?;
            Ok(u32::from_le_bytes(bytes))
        };
        let (width, height, fps) = (n()?, n()?, n()?);
        let mut video = [0];
        input.read_exact(&mut video)?;
        let video = match video[0] {
            1 => VideoCodec::H264,
            2 => VideoCodec::Hevc,
            other => bail!("an unknown picture codec {other}"),
        };
        let mut sound = [0];
        input.read_exact(&mut sound)?;
        let sound = match sound[0] {
            0 => None,
            1 => Some(SoundCodec::Aac),
            2 => Some(SoundCodec::Pcm),
            other => bail!("an unknown sound codec {other}"),
        };
        Ok(Self {
            width,
            height,
            fps,
            video,
            sound,
        })
    }
}

/// The largest frame accepted: a 4K key frame at a high bit rate is well
/// under this.
const MAX_FRAME: u32 = 32 << 20;

impl LiveFrame {
    pub fn write_to(&self, out: &mut impl Write) -> Result<()> {
        let track = match self.track {
            Track::Video => 1,
            Track::Sound => 2,
        };
        out.write_all(&[track, u8::from(self.key)])?;
        out.write_all(&self.time.to_le_bytes())?;
        out.write_all(&self.duration.to_le_bytes())?;
        out.write_all(&(self.data.len() as u32).to_le_bytes())?;
        out.write_all(&self.data)?;
        Ok(())
    }

    /// The next frame, or `None` at the end of the stream.
    pub fn read_from(input: &mut impl Read) -> Result<Option<Self>> {
        let mut head = [0; 22];
        match input.read_exact(&mut head) {
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            other => other?,
        }
        let track = match head[0] {
            1 => Track::Video,
            2 => Track::Sound,
            other => bail!("an unknown track {other}"),
        };
        let time = i64::from_le_bytes(head[2..10].try_into()?);
        let duration = i64::from_le_bytes(head[10..18].try_into()?);
        let len = u32::from_le_bytes(head[18..22].try_into()?);
        ensure!(len <= MAX_FRAME, "a frame of {len} bytes");
        let mut data = vec![0; len as usize];
        input.read_exact(&mut data)?;
        Ok(Some(Self {
            track,
            time,
            duration,
            key: head[1] != 0,
            data,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_format_and_frames_cross_the_wire_unchanged() {
        let format = LiveFormat {
            width: 2560,
            height: 1440,
            fps: 60,
            video: VideoCodec::Hevc,
            sound: Some(SoundCodec::Pcm),
        };
        let frames = [
            LiveFrame {
                track: Track::Video,
                time: 0,
                duration: 166_666,
                key: true,
                data: vec![0, 0, 0, 1, 0x67, 1, 2],
            },
            LiveFrame {
                track: Track::Sound,
                time: 213_333,
                duration: 213_333,
                key: true,
                data: vec![0xFF; 300],
            },
        ];
        let mut wire = Vec::new();
        format.write_to(&mut wire).unwrap();
        for frame in &frames {
            frame.write_to(&mut wire).unwrap();
        }
        let mut input = wire.as_slice();
        assert_eq!(LiveFormat::read_from(&mut input).unwrap(), format);
        for frame in &frames {
            assert_eq!(
                LiveFrame::read_from(&mut input).unwrap().as_ref(),
                Some(frame)
            );
        }
        assert_eq!(LiveFrame::read_from(&mut input).unwrap(), None);
    }

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
