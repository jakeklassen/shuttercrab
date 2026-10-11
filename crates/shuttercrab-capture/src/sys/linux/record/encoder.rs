//! Encoding on Linux, through GStreamer: frames go in through an `appsrc`,
//! are converted and encoded to H.264, and written to an MP4.
//!
//! GStreamer itself comes with every desktop; its H.264 encoder is in a
//! plugin package that may be missing ([`missing_software`] names it).

use anyhow::{Context, Result, anyhow, bail};
use gstreamer::{self as gst, prelude::*};
use gstreamer_app::AppSrc;
use std::{path::Path, time::Duration};

/// The H.264 encoders Shuttercrab can use, best first, with the parser
/// each needs before the MP4 muxer, if any.
const ENCODERS: [(&str, Option<&str>); 2] = [("x264enc", None), ("openh264enc", Some("h264parse"))];

/// A GStreamer element and the plugin package it comes in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Plugins {
    Base,
    Good,
    H264,
}

/// The elements recording needs other than the encoder.
const ELEMENTS: [(&str, Plugins); 3] = [
    ("appsrc", Plugins::Base),
    ("videoconvert", Plugins::Base),
    ("mp4mux", Plugins::Good),
];

/// What recording needs that is not installed, as the user reads it: what
/// is missing and the command that installs it. `None` when nothing is.
pub fn missing_software() -> Option<String> {
    if let Err(e) = gst::init() {
        log::warn!("GStreamer did not start: {e}");
        return Some(install_message(&[Plugins::Base]));
    }
    let mut missing: Vec<Plugins> = ELEMENTS
        .iter()
        .filter(|(name, _)| gst::ElementFactory::find(name).is_none())
        .map(|&(_, plugins)| plugins)
        .collect();
    if video_encoder().is_none() {
        missing.push(Plugins::H264);
    }
    missing.dedup();
    if missing.is_empty() {
        None
    } else {
        log::warn!("recording needs GStreamer plugins: {missing:?}");
        Some(install_message(&missing))
    }
}

/// The encoder to use and its parser, if all are installed.
fn video_encoder() -> Option<(&'static str, Option<&'static str>)> {
    ENCODERS.into_iter().find(|(encoder, parser)| {
        gst::ElementFactory::find(encoder).is_some()
            && parser.is_none_or(|p| gst::ElementFactory::find(p).is_some())
    })
}

/// "Recording needs GStreamer's H.264 encoder. Install it with: sudo apt
/// install gstreamer1.0-plugins-ugly", in this distribution's words.
fn install_message(missing: &[Plugins]) -> String {
    let what = if missing == [Plugins::H264] {
        "GStreamer's H.264 encoder"
    } else {
        "some of GStreamer's plugins"
    };
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    match install_command(&release, missing) {
        Some(command) => format!("Recording needs {what}. Install it with: {command}"),
        None => format!(
            "Recording needs {what}: the base, good and ugly plugin sets (or OpenH264). Install them with your package manager."
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
    let (command, names): (&str, [&str; 3]) = if is("debian") || is("ubuntu") {
        (
            "sudo apt install",
            [
                "gstreamer1.0-plugins-base",
                "gstreamer1.0-plugins-good",
                "gstreamer1.0-plugins-ugly",
            ],
        )
    } else if is("fedora") {
        (
            "sudo dnf install",
            [
                "gstreamer1-plugins-base",
                "gstreamer1-plugins-good",
                "gstreamer1-plugin-openh264 gstreamer1-plugins-bad-free",
            ],
        )
    } else if is("arch") {
        (
            "sudo pacman -S",
            ["gst-plugins-base", "gst-plugins-good", "gst-plugins-ugly"],
        )
    } else {
        return None;
    };
    let packages: Vec<&str> = missing
        .iter()
        .map(|plugins| match plugins {
            Plugins::Base => names[0],
            Plugins::Good => names[1],
            Plugins::H264 => names[2],
        })
        .collect();
    Some(format!("{command} {}", packages.join(" ")))
}

/// An MP4 being written: frames go in, H.264 comes out.
pub(super) struct Encoder {
    pipeline: gst::Pipeline,
    source: AppSrc,
}

impl Encoder {
    /// Start writing `path`, for BGRx frames of `width` × `height` at `fps`.
    pub(super) fn start(path: &Path, (width, height): (u32, u32), fps: u32) -> Result<Self> {
        gst::init().context("starting GStreamer")?;
        if let Some(message) = missing_software() {
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
        chain.extend([mux, sink]);
        pipeline.add_many(&chain)?;
        gst::Element::link_many(&chain).context("linking the recording pipeline")?;
        pipeline
            .set_state(gst::State::Playing)
            .context("starting the recording pipeline")?;
        let encoder = Self { pipeline, source };
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

    /// Fail if the pipeline reported an error.
    fn check(&self) -> Result<()> {
        let bus = self.pipeline.bus().context("no pipeline bus")?;
        match bus.pop_filtered(&[gst::MessageType::Error]) {
            Some(message) => Err(pipeline_error(&message)),
            None => Ok(()),
        }
    }

    /// Finish the file: the encoder empties out and the MP4 gets its index.
    pub(super) fn finish(self) -> Result<()> {
        self.source
            .end_of_stream()
            .map_err(|e| anyhow!("could not end the recording: {e:?}"))?;
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
            install_command("ID=fedora\n", &[Plugins::H264]).unwrap(),
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
