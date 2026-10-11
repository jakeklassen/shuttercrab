//! Encoding on Linux, through GStreamer: frames go in through an `appsrc`,
//! are converted and encoded to H.264, and written to an MP4; the mixed
//! sound goes in through another and is encoded to AAC beside them.
//!
//! GStreamer itself comes with every desktop; its H.264 and AAC encoders
//! are in plugin packages that may be missing ([`missing_software`] names
//! them).

use crate::record::mix::{CHANNELS, RATE, frames_to_ticks};
use anyhow::{Context, Result, anyhow, bail};
use gstreamer::{self as gst, prelude::*};
use gstreamer_app::AppSrc;
use std::{path::Path, time::Duration};

/// The H.264 encoders Shuttercrab can use, best first, with the parser
/// each needs before the MP4 muxer, if any.
const ENCODERS: [(&str, Option<&str>); 2] = [("x264enc", None), ("openh264enc", Some("h264parse"))];

/// The AAC encoders, best first.
const SOUND_ENCODERS: [&str; 3] = ["fdkaacenc", "avenc_aac", "voaacenc"];

/// The sound track's bit rate: 192 kbit/s, as on Windows.
const SOUND_BITRATE: u32 = 192_000;

/// A GStreamer element and the plugin package it comes in.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Plugins {
    Base,
    Good,
    H264,
    Aac,
}

/// The elements recording needs other than the encoder.
const ELEMENTS: [(&str, Plugins); 3] = [
    ("appsrc", Plugins::Base),
    ("videoconvert", Plugins::Base),
    ("mp4mux", Plugins::Good),
];

/// The elements recording sound needs other than the encoder.
const SOUND_ELEMENTS: [(&str, Plugins); 5] = [
    ("pulsesrc", Plugins::Good),
    ("appsink", Plugins::Base),
    ("audioconvert", Plugins::Base),
    ("audioresample", Plugins::Base),
    ("aacparse", Plugins::Good),
];

/// What recording needs that is not installed, as the user reads it: what
/// is missing and the command that installs it. `None` when nothing is.
/// `sound`: whether a sound source is switched on.
pub fn missing_software(sound: bool) -> Option<String> {
    if let Err(e) = gst::init() {
        log::warn!("GStreamer did not start: {e}");
        return Some(install_message(&[Plugins::Base]));
    }
    let mut missing = missing_of(&ELEMENTS);
    if video_encoder().is_none() {
        missing.push(Plugins::H264);
    }
    if sound {
        missing.extend(missing_for_sound());
    }
    missing.sort();
    missing.dedup();
    if missing.is_empty() {
        None
    } else {
        log::warn!("recording needs GStreamer plugins: {missing:?}");
        Some(install_message(&missing))
    }
}

/// Whether the sound track can be recorded.
pub(super) fn sound_installed() -> bool {
    gst::init().is_ok() && missing_for_sound().is_empty()
}

/// The plugins of `elements` that are not installed.
fn missing_of(elements: &[(&str, Plugins)]) -> Vec<Plugins> {
    elements
        .iter()
        .filter(|(name, _)| gst::ElementFactory::find(name).is_none())
        .map(|&(_, plugins)| plugins)
        .collect()
}

/// What the sound track needs that is not installed.
fn missing_for_sound() -> Vec<Plugins> {
    let mut missing = missing_of(&SOUND_ELEMENTS);
    if sound_encoder().is_none() {
        missing.push(Plugins::Aac);
    }
    missing
}

/// The encoder to use and its parser, if all are installed.
fn video_encoder() -> Option<(&'static str, Option<&'static str>)> {
    ENCODERS.into_iter().find(|(encoder, parser)| {
        gst::ElementFactory::find(encoder).is_some()
            && parser.is_none_or(|p| gst::ElementFactory::find(p).is_some())
    })
}

/// The AAC encoder to use, if one is installed.
fn sound_encoder() -> Option<&'static str> {
    SOUND_ENCODERS
        .into_iter()
        .find(|name| gst::ElementFactory::find(name).is_some())
}

/// "Recording needs GStreamer's H.264 encoder. Install it with: sudo apt
/// install gstreamer1.0-plugins-ugly", in this distribution's words.
fn install_message(missing: &[Plugins]) -> String {
    let what = match missing {
        [Plugins::H264] => "GStreamer's H.264 encoder",
        [Plugins::Aac] => "GStreamer's AAC encoder for sound",
        _ => "some of GStreamer's plugins",
    };
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    match install_command(&release, missing) {
        Some(command) => format!("Recording needs {what}. Install it with: {command}"),
        None => format!(
            "Recording needs {what}: the base, good and ugly plugin sets (or OpenH264), and for sound the libav plugins. Install them with your package manager."
        ),
    }
}

/// The command that installs `missing` on the distribution `os-release`
/// describes, for the families with known package names.
fn install_command(release: &str, missing: &[Plugins]) -> Option<String> {
    let field = |key: &str| {
        release
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(|value| value.trim_matches('"').to_ascii_lowercase())
            .unwrap_or_default()
    };
    let family = format!("{} {}", field("ID"), field("ID_LIKE"));
    let is = |name: &str| family.split_whitespace().any(|word| word == name);
    // Base, Good, H264 and Aac's packages.
    let (command, names): (&str, [&str; 4]) = if is("debian") || is("ubuntu") {
        (
            "sudo apt install",
            [
                "gstreamer1.0-plugins-base",
                "gstreamer1.0-plugins-good",
                "gstreamer1.0-plugins-ugly",
                "gstreamer1.0-libav",
            ],
        )
    } else if is("fedora") {
        (
            "sudo dnf install",
            [
                "gstreamer1-plugins-base",
                "gstreamer1-plugins-good",
                "gstreamer1-plugin-openh264 gstreamer1-plugins-bad-free",
                "gstreamer1-plugins-bad-free",
            ],
        )
    } else if is("arch") {
        (
            "sudo pacman -S",
            [
                "gst-plugins-base",
                "gst-plugins-good",
                "gst-plugins-ugly",
                "gst-libav",
            ],
        )
    } else {
        return None;
    };
    let mut packages: Vec<&str> = Vec::new();
    for plugins in missing {
        let name = names[*plugins as usize];
        for package in name.split_whitespace() {
            if !packages.contains(&package) {
                packages.push(package);
            }
        }
    }
    Some(format!("{command} {}", packages.join(" ")))
}

/// An MP4 being written: frames go in, H.264 comes out, and the mixed
/// sound as AAC beside it if the file has a sound track.
pub(super) struct Encoder {
    pipeline: gst::Pipeline,
    source: AppSrc,
    sound: Option<AppSrc>,
}

impl Encoder {
    /// Start writing `path`, for BGRx frames of `width` × `height` at `fps`,
    /// with a sound track (48 kHz stereo AAC) if `sound` and it can be
    /// recorded.
    pub(super) fn start(
        path: &Path,
        (width, height): (u32, u32),
        fps: u32,
        sound: bool,
    ) -> Result<Self> {
        gst::init().context("starting GStreamer")?;
        if let Some(message) = missing_software(false) {
            bail!("{message}");
        }
        let (encoder_name, parser) = video_encoder().context("no H.264 encoder")?;
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "BGRx")
            .field("width", width as i32)
            .field("height", height as i32)
            .field("framerate", gst::Fraction::new(fps as i32, 1))
            .build();
        let frame_bytes = u64::from(width) * u64::from(height) * 4;
        // A few frames of slack; past that, pushing waits for the encoder,
        // and the frames that would have come meanwhile are skipped.
        let source = AppSrc::builder()
            .caps(&caps)
            .format(gst::Format::Time)
            .is_live(true)
            .block(true)
            .max_bytes(frame_bytes * 4)
            .build();
        let convert = gst::ElementFactory::make("videoconvert").build()?;
        // 4:2:0 in BT.709, as on Windows. Left to choose, x264 takes 4:4:4,
        // which browsers, chat apps and most players cannot play.
        let yuv = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("format", "I420")
                    .field("colorimetry", "bt709")
                    .build(),
            )
            .build()?;
        let bitrate = bitrate((width, height), fps);
        let encoder = match encoder_name {
            // Zero latency: no look-ahead or B-frames to hold frames for.
            // At 1080p that is 80 MB of memory rather than 650, and it
            // still encodes three times faster than real time.
            "x264enc" => gst::ElementFactory::make("x264enc")
                .property("bitrate", bitrate / 1000)
                .property_from_str("speed-preset", "veryfast")
                .property_from_str("tune", "zerolatency")
                .property("key-int-max", fps * 2)
                .build()?,
            _ => gst::ElementFactory::make(encoder_name)
                .property("bitrate", bitrate)
                .build()?,
        };
        log::info!("recording with {encoder_name} at {} kbit/s", bitrate / 1000);
        let mux = gst::ElementFactory::make("mp4mux").build()?;
        let sink = gst::ElementFactory::make("filesink")
            .property("location", path.to_string_lossy().as_ref())
            .build()?;
        let pipeline = gst::Pipeline::new();
        let mut chain = vec![
            source.upcast_ref::<gst::Element>().clone(),
            convert,
            yuv,
            encoder,
        ];
        if let Some(parser) = parser {
            chain.push(gst::ElementFactory::make(parser).build()?);
        }
        chain.extend([encoded_queue()?, mux.clone(), sink]);
        pipeline.add_many(&chain)?;
        gst::Element::link_many(&chain).context("linking the recording pipeline")?;
        let sound = if !sound {
            None
        } else if sound_installed() {
            Some(add_sound(&pipeline, &mux)?)
        } else {
            log::warn!("recording without a sound track: GStreamer's sound plugins are missing");
            None
        };
        pipeline
            .set_state(gst::State::Playing)
            .context("starting the recording pipeline")?;
        let encoder = Self {
            pipeline,
            source,
            sound,
        };
        encoder.check()?;
        Ok(encoder)
    }

    /// Encode `frame` (BGRx) at `at` on the recording's timeline, showing
    /// for `duration`.
    pub(super) fn push(&self, frame: Vec<u8>, at: Duration, duration: Duration) -> Result<()> {
        let mut buffer = gst::Buffer::from_mut_slice(frame);
        let timing = buffer.get_mut().context("a new buffer is shared")?;
        timing.set_pts(clock_time(at));
        timing.set_duration(clock_time(duration));
        self.source
            .push_buffer(buffer)
            .map_err(|e| anyhow!("the encoder took no more frames: {e:?}"))?;
        self.check()
    }

    /// Whether the file has a sound track.
    pub(super) fn has_sound(&self) -> bool {
        self.sound.is_some()
    }

    /// Hand `samples` (16-bit, the channels interleaved) to the sound track
    /// at `time` (100 ns ticks on the recording's timeline).
    pub(super) fn push_sound(&self, samples: &[i16], time: i64) -> Result<()> {
        let Some(sound) = &self.sound else {
            return Ok(());
        };
        if samples.is_empty() {
            return Ok(());
        }
        let frames = samples.len() as u64 / u64::from(CHANNELS);
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut buffer = gst::Buffer::from_mut_slice(bytes);
        let timing = buffer.get_mut().context("a new buffer is shared")?;
        timing.set_pts(ticks_time(time));
        timing.set_duration(ticks_time(frames_to_ticks(frames)));
        sound
            .push_buffer(buffer)
            .map_err(|e| anyhow!("the encoder took no more sound: {e:?}"))?;
        self.check()
    }

    /// Fail if the pipeline reported an error.
    fn check(&self) -> Result<()> {
        let bus = self.pipeline.bus().context("no pipeline bus")?;
        match bus.pop_filtered(&[gst::MessageType::Error]) {
            Some(message) => Err(pipeline_error(&message)),
            None => Ok(()),
        }
    }

    /// Finish the file: the encoders empty out and the MP4 gets its index.
    pub(super) fn finish(self) -> Result<()> {
        for source in std::iter::once(&self.source).chain(&self.sound) {
            source
                .end_of_stream()
                .map_err(|e| anyhow!("could not end the recording: {e:?}"))?;
        }
        let bus = self.pipeline.bus().context("no pipeline bus")?;
        let message = bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(30),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        match message {
            Some(m) if m.type_() == gst::MessageType::Eos => Ok(()),
            Some(m) => Err(pipeline_error(&m)),
            None => Err(anyhow!("the recording took too long to finish")),
        }
    }
}

/// Add the sound track's branch to `pipeline`, into `mux`: mixed sound in
/// through the source returned, AAC out.
fn add_sound(pipeline: &gst::Pipeline, mux: &gst::Element) -> Result<AppSrc> {
    let name = sound_encoder().context("no AAC encoder")?;
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("layout", "interleaved")
        .field("rate", RATE as i32)
        .field("channels", i32::from(CHANNELS))
        .build();
    // The mix comes thirty times a second, a little behind the picture;
    // it is never held up.
    let source = AppSrc::builder()
        .caps(&caps)
        .format(gst::Format::Time)
        .is_live(true)
        .max_bytes(0)
        .build();
    let encoder = gst::ElementFactory::make(name).build()?;
    // An integer of one width or another, depending on the encoder.
    encoder.set_property_from_str("bitrate", &SOUND_BITRATE.to_string());
    log::info!(
        "recording sound with {name} at {} kbit/s",
        SOUND_BITRATE / 1000
    );
    let queue = encoded_queue()?;
    let chain = [
        source.upcast_ref::<gst::Element>().clone(),
        gst::ElementFactory::make("audioconvert").build()?,
        encoder,
        gst::ElementFactory::make("aacparse").build()?,
        queue.clone(),
    ];
    pipeline.add_many(&chain)?;
    gst::Element::link_many(&chain).context("linking the sound track")?;
    queue
        .link(mux)
        .context("adding the sound track to the MP4")?;
    Ok(source)
}

/// A queue for an encoded track before the muxer, which waits for the
/// other track to catch up: the sound comes a fifth of a second behind
/// the picture. Encoded, that is little memory.
fn encoded_queue() -> Result<gst::Element> {
    Ok(gst::ElementFactory::make("queue")
        .property("max-size-buffers", 0u32)
        .property("max-size-bytes", 0u32)
        .property("max-size-time", gst::ClockTime::from_seconds(10).nseconds())
        .build()?)
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn pipeline_error(message: &gst::Message) -> anyhow::Error {
    match message.view() {
        gst::MessageView::Error(e) => anyhow!(
            "{} ({})",
            e.error(),
            e.debug().map(|d| d.to_string()).unwrap_or_default()
        ),
        _ => anyhow!("the recording pipeline failed"),
    }
}

fn clock_time(duration: Duration) -> gst::ClockTime {
    gst::ClockTime::from_nseconds(duration.as_nanos() as u64)
}

/// 100 ns ticks (none if negative) as GStreamer time.
fn ticks_time(ticks: i64) -> gst::ClockTime {
    gst::ClockTime::from_nseconds(ticks.max(0) as u64 * 100)
}

/// Bits per second: a tenth of a bit per pixel per frame, 2 to 80 Mbit/s,
/// as on Windows.
fn bitrate((width, height): (u32, u32), fps: u32) -> u32 {
    let bits = u64::from(width) * u64::from(height) * u64::from(fps) / 10;
    bits.clamp(2_000_000, 80_000_000) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_plugins_name_this_distributions_packages() {
        let ubuntu = "NAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\n";
        assert_eq!(
            install_command(ubuntu, &[Plugins::H264]).unwrap(),
            "sudo apt install gstreamer1.0-plugins-ugly"
        );
        let mint = "ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n";
        assert_eq!(
            install_command(mint, &[Plugins::Good, Plugins::H264]).unwrap(),
            "sudo apt install gstreamer1.0-plugins-good gstreamer1.0-plugins-ugly"
        );
        assert_eq!(
            install_command(ubuntu, &[Plugins::Aac]).unwrap(),
            "sudo apt install gstreamer1.0-libav"
        );
        // A package is named once, whatever needs it.
        assert_eq!(
            install_command("ID=fedora\n", &[Plugins::H264, Plugins::Aac]).unwrap(),
            "sudo dnf install gstreamer1-plugin-openh264 gstreamer1-plugins-bad-free"
        );
        assert_eq!(
            install_command("ID=endeavouros\nID_LIKE=arch\n", &[Plugins::Base]).unwrap(),
            "sudo pacman -S gst-plugins-base"
        );
        assert!(install_command("ID=gentoo\n", &[Plugins::H264]).is_none());
    }

    #[test]
    fn the_bitrate_follows_the_size_like_windows() {
        assert_eq!(bitrate((1920, 1080), 30), 6_220_800);
        assert_eq!(bitrate((320, 240), 30), 2_000_000);
        assert_eq!(bitrate((7680, 4320), 60), 80_000_000);
    }
}
