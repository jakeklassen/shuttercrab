//! How a live recording crosses the network: what it is ([`LiveFormat`]),
//! sent first, then its coded frames ([`LiveFrame`]), one after another.
//! Numbers are little-endian.

use anyhow::{Result, bail, ensure};
use shuttercrab_capture::live::{LiveFormat, LiveFrame, SoundCodec, Track, VideoCodec};
use std::io::{Read, Write};

/// The first byte of each stream online, so the viewer can tell them
/// apart: the format and the pictures, and the sound.
pub(crate) const PICTURES: u8 = b'P';
pub(crate) const SOUND: u8 = b'S';

/// "Shuttercrab live", version 2: the pictures' codec follows the frame rate.
const MAGIC: [u8; 4] = *b"SCL2";

/// The largest frame accepted: a 4K key frame at a high bit rate is well
/// under this.
const MAX_FRAME: u32 = 32 << 20;

pub(crate) fn write_format(format: &LiveFormat, out: &mut impl Write) -> Result<()> {
    out.write_all(&MAGIC)?;
    for n in [format.width, format.height, format.fps] {
        out.write_all(&n.to_le_bytes())?;
    }
    out.write_all(&[match format.video {
        VideoCodec::H264 => 1,
        VideoCodec::Hevc => 2,
        VideoCodec::Av1 => 3,
    }])?;
    let sound = match format.sound {
        None => 0,
        Some(SoundCodec::Aac) => 1,
        Some(SoundCodec::Pcm) => 2,
        Some(SoundCodec::Opus) => 3,
    };
    out.write_all(&[sound])?;
    Ok(())
}

pub(crate) fn read_format(input: &mut impl Read) -> Result<LiveFormat> {
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
        3 => VideoCodec::Av1,
        other => bail!("an unknown picture codec {other}"),
    };
    let mut sound = [0];
    input.read_exact(&mut sound)?;
    let sound = match sound[0] {
        0 => None,
        1 => Some(SoundCodec::Aac),
        2 => Some(SoundCodec::Pcm),
        3 => Some(SoundCodec::Opus),
        other => bail!("an unknown sound codec {other}"),
    };
    Ok(LiveFormat {
        width,
        height,
        fps,
        video,
        sound,
    })
}

pub(crate) fn write_frame(frame: &LiveFrame, out: &mut impl Write) -> Result<()> {
    let track = match frame.track {
        Track::Video => 1,
        Track::Sound => 2,
    };
    out.write_all(&[track, u8::from(frame.key)])?;
    out.write_all(&frame.time.to_le_bytes())?;
    out.write_all(&frame.duration.to_le_bytes())?;
    out.write_all(&(frame.data.len() as u32).to_le_bytes())?;
    out.write_all(&frame.data)?;
    Ok(())
}

/// The next frame, or `None` at the end of the stream.
pub(crate) fn read_frame(input: &mut impl Read) -> Result<Option<LiveFrame>> {
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
    Ok(Some(LiveFrame {
        track,
        time,
        duration,
        key: head[1] != 0,
        data,
    }))
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
            video: VideoCodec::Av1,
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
        write_format(&format, &mut wire).unwrap();
        for frame in &frames {
            write_frame(frame, &mut wire).unwrap();
        }
        let mut input = wire.as_slice();
        assert_eq!(read_format(&mut input).unwrap(), format);
        for frame in &frames {
            assert_eq!(read_frame(&mut input).unwrap().as_ref(), Some(frame));
        }
        assert_eq!(read_frame(&mut input).unwrap(), None);
    }
}
