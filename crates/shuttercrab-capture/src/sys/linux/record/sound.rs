//! A recording's sound on Linux: what the speakers play (the default
//! output's monitor) and a microphone, from PulseAudio, or PipeWire's
//! PulseAudio service, each through a GStreamer pipeline of its own while
//! switched on.
//!
//! GStreamer converts each to the track's format (48 kHz, 16-bit stereo),
//! whatever the device runs at, and hands each packet over as it comes,
//! stamped with when it was captured. The recording thread mixes them with
//! a [`Mixer`] and writes the mix out a little behind the picture. A
//! microphone switched off is not opened at all, so the system does not
//! show it as in use.

use super::{Event, encoder::Encoder};
use crate::record::{
    Microphone, RecordOptions, Source,
    mix::{CHANNELS, Mixer, RATE, frames_to_ticks},
};
use anyhow::{Context, Result, anyhow};
use gstreamer::{self as gst, prelude::*};
use gstreamer_app::{AppSink, AppSinkCallbacks};
use std::{
    sync::mpsc::Sender,
    time::{Duration, Instant},
};

/// What the speakers play: the default output's monitor, in PulseAudio's
/// words.
const SPEAKERS: &str = "@DEFAULT_MONITOR@";

/// The microphones plugged in, the system's default first.
pub fn microphones() -> Result<Vec<Microphone>> {
    gst::init().context("starting GStreamer")?;
    let provider = gst::DeviceProviderFactory::by_name("pulsedeviceprovider")
        .context("GStreamer's PulseAudio plugin is missing")?;
    let mut found: Vec<(bool, Microphone)> = provider
        .devices()
        .into_iter()
        .filter(|device| device.has_classes("Audio/Source"))
        .filter_map(|device| {
            let properties = device.properties()?;
            // A monitor is an output's sound, not a microphone.
            let class = properties.get::<String>("device.class").ok();
            if class.as_deref() == Some("monitor") {
                return None;
            }
            device.find_property("internal-name")?;
            let id = device.property::<Option<String>>("internal-name")?;
            let default = properties.get::<bool>("is-default").unwrap_or(false);
            Some((
                default,
                Microphone {
                    id,
                    name: device.display_name().into(),
                },
            ))
        })
        .collect();
    found.sort_by_key(|(default, _)| !default);
    Ok(found
        .into_iter()
        .map(|(_, microphone)| microphone)
        .collect())
}

/// Sound from one source, captured at `captured`: 16-bit samples, the
/// channels interleaved.
pub(super) struct Packet {
    pub(super) source: Source,
    pub(super) captured: Instant,
    pub(super) samples: Vec<i16>,
}

/// One source's capture, running until dropped. GStreamer calls back on a
/// thread of its own with each packet.
struct Capture {
    pipeline: gst::Pipeline,
}

impl Capture {
    /// Start capturing `source`, from `microphone` (PulseAudio's name for
    /// it) or the default one, sending each packet to the recording thread
    /// as [`Event::Sound`]. Returns once the capture is running, or why it
    /// could not start.
    fn start(source: Source, microphone: Option<&str>, events: Sender<Event>) -> Result<Self> {
        gst::init().context("starting GStreamer")?;
        let input = gst::ElementFactory::make("pulsesrc")
            .property("client-name", "Shuttercrab")
            .build()?;
        let device = match source {
            Source::System => Some(SPEAKERS),
            Source::Microphone => microphone,
        };
        if let Some(device) = device {
            input.set_property("device", device);
        }
        let caps = gst::Caps::builder("audio/x-raw")
            .field("format", "S16LE")
            .field("layout", "interleaved")
            .field("rate", RATE as i32)
            .field("channels", i32::from(CHANNELS))
            .build();
        let sink = AppSink::builder().caps(&caps).sync(false).build();
        sink.set_callbacks(
            AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let packet = packet(source, &sample).ok_or(gst::FlowError::Error)?;
                    // The recording has ended.
                    events
                        .send(Event::Sound(packet))
                        .map_err(|_| gst::FlowError::Eos)?;
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        let pipeline = gst::Pipeline::new();
        let chain = [
            input,
            gst::ElementFactory::make("audioconvert").build()?,
            gst::ElementFactory::make("audioresample").build()?,
            sink.upcast_ref::<gst::Element>().clone(),
        ];
        pipeline.add_many(&chain)?;
        gst::Element::link_many(&chain).context("linking the sound capture")?;
        let bus = pipeline.bus().context("no pipeline bus")?;
        let capture = Self { pipeline };
        // PulseAudio is reached as the capture starts: no server, or no
        // such device, fails here.
        let started = capture.pipeline.set_state(gst::State::Playing);
        if let Some(message) = bus.pop_filtered(&[gst::MessageType::Error]) {
            return Err(error_of(&message)).context("could not start capturing sound");
        }
        started.context("could not start capturing sound")?;
        // Mid-recording, a failure (the device gone) leaves that source's
        // part of the track silent.
        bus.set_sync_handler(move |_, message| {
            if let gst::MessageView::Error(_) = message.view() {
                log::warn!("{source:?} sound stopped: {:#}", error_of(message));
            }
            gst::BusSyncReply::Drop
        });
        Ok(capture)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// `sample` as a packet from `source`, stamped with when it was captured:
/// it has just arrived, so its first frame was captured its length ago.
fn packet(source: Source, sample: &gst::Sample) -> Option<Packet> {
    let buffer = sample.buffer()?;
    let map = buffer.map_readable().ok()?;
    let samples: Vec<i16> = map
        .as_slice()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| i16::from_le_bytes(*bytes))
        .collect();
    let frames = (samples.len() / usize::from(CHANNELS)) as u64;
    let length = Duration::from_nanos(frames_to_ticks(frames) as u64 * 100);
    let now = Instant::now();
    Some(Packet {
        source,
        captured: now.checked_sub(length).unwrap_or(now),
        samples,
    })
}

fn error_of(message: &gst::Message) -> anyhow::Error {
    match message.view() {
        gst::MessageView::Error(e) => anyhow!(
            "{} ({})",
            e.error(),
            e.debug().map(|d| d.to_string()).unwrap_or_default()
        ),
        _ => anyhow!("the sound capture failed"),
    }
}

/// The sound track being recorded: each source's capture while switched
/// on, and their mix.
pub(super) struct Sound {
    captures: [Option<Capture>; 2],
    mixer: Mixer,
    /// Where captures send their sound, to start one later.
    events: Sender<Event>,
    /// The microphone to record, or the default one.
    microphone: Option<String>,
}

impl Sound {
    /// Start capturing the sources `options` switch on. A source that
    /// cannot start leaves its part of the track silent: the rest still
    /// records.
    pub(super) fn start(options: &RecordOptions, events: Sender<Event>) -> Self {
        if options.sound_process.is_some() {
            log::info!(
                "recording all of the speakers' sound: one app's alone is not possible here"
            );
        }
        let mut sound = Self {
            captures: [None, None],
            mixer: Mixer::default(),
            events,
            microphone: options.microphone_device.clone(),
        };
        sound.set(Source::System, options.system_sound);
        sound.set(Source::Microphone, options.microphone);
        sound
    }

    /// Switch `source` on or off.
    pub(super) fn set(&mut self, source: Source, on: bool) {
        let slot = &mut self.captures[index(source)];
        match (on, slot.is_some()) {
            (true, false) => {
                *slot = Capture::start(source, self.microphone.as_deref(), self.events.clone())
                    .or_else(|e| match (source, self.microphone.take()) {
                        // The chosen microphone is not there: the default one.
                        (Source::Microphone, Some(chosen)) => {
                            log::warn!("no microphone {chosen}: {e:#}; recording the default one");
                            Capture::start(source, None, self.events.clone())
                        }
                        _ => Err(e),
                    })
                    .inspect_err(|e| log::warn!("recording without {source:?}: {e:#}"))
                    .ok();
            }
            (false, true) => *slot = None,
            _ => return,
        }
        log::info!("{source:?} {}", if on { "on" } else { "off" });
    }

    /// Mix `packet` in at `time` (100 ns ticks on the recording's
    /// timeline).
    pub(super) fn place(&mut self, packet: &Packet, time: i64) {
        self.mixer.add(packet.source, time, &packet.samples);
    }

    /// Write the mix out up to `time` (100 ns ticks on the recording's
    /// timeline): silence where no source had sound.
    pub(super) fn write_until(&mut self, encoder: &Encoder, time: i64) -> Result<()> {
        let first = frames_to_ticks(self.mixer.written());
        let samples = self.mixer.take_until(time);
        encoder.push_sound(&samples, first)
    }

    /// Stop capturing: the track ends with the picture. Says in the log how
    /// much sound came too late to mix, if any did.
    pub(super) fn stop(&mut self) {
        self.captures = [None, None];
        for source in [Source::System, Source::Microphone] {
            let late = self.mixer.late(source);
            if late > 0 {
                log::info!("{source:?} sound: {} ms too late to mix", late / 10_000);
            }
        }
    }
}

fn index(source: Source) -> usize {
    match source {
        Source::System => 0,
        Source::Microphone => 1,
    }
}
