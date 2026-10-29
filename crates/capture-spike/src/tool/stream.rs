//! Live streaming, spikes S0 and S1 (issue #66), from the terminal: a
//! recording streamed as it is recorded, to a viewer on this PC or, through
//! the meeting point, anywhere; and a window that plays it as it arrives.
//! The work is in `shuttercrab-stream`; this reads the options and prints
//! how it goes.

use super::Args;
use anyhow::{Context, Result};
use capture_spike::{analysis::Roi, display};
use shuttercrab_capture::{
    MonitorId, PhysicalRect,
    live::{SoundCodec, VideoCodec},
    record::RecordOptions,
};
use shuttercrab_stream::{Input, Link, Quality, Room, Stats, Stream, WatchOptions, describe};
use std::time::{Duration, Instant};

const DEFAULT_PORT: u16 = 47_100;

/// The recorder's, the player's and the connection's log, on stderr: how
/// late each source's sound arrived and how much came too late to mix;
/// when the played sound ran dry or was cut; who connected.
struct Log;

impl log::Log for Log {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("{}: {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

pub(super) fn send(mut args: Args) -> Result<()> {
    let monitor = args.option("--monitor")?;
    let region = args
        .option("--region")?
        .map(|r| Roi::parse(&r))
        .transpose()?;
    let fps: u32 = args
        .option("--fps")?
        .map(|f| f.parse())
        .transpose()?
        .unwrap_or(60);
    let quality = match args.option("--quality")? {
        Some(q) => Quality::parse(&q).context("--quality takes 1080p, 1440p or 4k")?,
        None => Quality::P1440,
    };
    let system_sound = args.option("--sound")?.as_deref() == Some("on");
    let sound_process = args
        .option("--sound-of")?
        .map(|p| p.parse::<u32>())
        .transpose()
        .context("--sound-of takes a process id")?;
    let microphone = args.option("--mic")?.as_deref() == Some("on");
    let (codec, video) = codecs(&mut args)?;
    let port: u16 = args
        .option("--port")?
        .map(|p| p.parse())
        .transpose()?
        .unwrap_or(DEFAULT_PORT);
    let online = args.option("--online")?.as_deref() == Some("on");
    let meet = Meet::from_args(&mut args)?;
    args.finish()?;
    let _ = log::set_logger(&Log);
    log::set_max_level(log::LevelFilter::Info);

    let monitors = display::enumerate()?;
    let target = display::select(&monitors, monitor.as_deref())?;
    // The whole monitor unless told otherwise.
    let region = region.unwrap_or(Roi {
        x: 0,
        y: 0,
        width: target.bounds.width,
        height: target.bounds.height,
    });
    let (width, height) = (region.width & !1, region.height & !1);
    let sound = system_sound || sound_process.is_some() || microphone;
    println!(
        "{width}x{height} of {} at {fps} fps, {} ({codec:?}).",
        target.device_name,
        describe_sound(sound_process, system_sound, microphone)
    );
    let viewer = if online {
        let room = Room::open(&meet.url, &host_key()?, meet.path)?;
        println!(
            "Share this link; whoever has it can watch:\n\n  {}\n\n\
             Waiting for a viewer (capture-spike stream-watch --link LINK)...",
            room.link()
        );
        room.accept()?
    } else {
        println!("Waiting for a viewer on 127.0.0.1:{port} (capture-spike stream-watch)...");
        shuttercrab_stream::wait_locally(port)?
    };
    let options = RecordOptions {
        monitor: MonitorId::from_raw(target.hmonitor.0 as u64),
        region: Some(PhysicalRect::new(
            region.x as i32,
            region.y as i32,
            width,
            height,
        )),
        window: None,
        fps,
        include_cursor: true,
        system_sound: system_sound || sound_process.is_some(),
        sound_process,
        microphone,
        microphone_device: None,
        sound_track: sound,
        path: "unused.mp4".into(),
    };
    let stream = Stream::start(options, (width, height), viewer, quality, (video, codec))?;
    println!("Pictures as {}.", describe(stream.video()));
    println!(
        "Streaming. 1, 2 and 3 switch to {}, {} and {}; close the viewer, or Ctrl+C here, to stop.",
        Quality::P1080.name(),
        Quality::P1440.name(),
        Quality::P2160.name()
    );
    run(stream)
}

/// Stream until the viewer leaves, switching quality as keys in the
/// terminal say, with a status line a second.
fn run(mut stream: Stream) -> Result<()> {
    let keys = keys();
    let mut last = Instant::now();
    while !stream.gone() {
        let wait = Duration::from_secs(1).saturating_sub(last.elapsed());
        if let Ok(key) = keys.recv_timeout(wait) {
            let chosen = key
                .to_digit(10)
                .and_then(|n| Quality::ALL.get((n as usize).checked_sub(1)?).copied());
            if let Some(quality) = chosen.filter(|&q| q != stream.quality()) {
                stream.set_quality(quality)?;
                println!(
                    "Now {} ({} Mbit/s).",
                    quality.name(),
                    quality.bitrate() / 1_000_000
                );
            }
            continue;
        }
        last = Instant::now();
        report(stream.elapsed(), &stream.stats());
    }
    let seconds = stream.elapsed().as_secs_f64();
    let summary = stream.stop()?;
    println!(
        "The viewer left. Streamed {seconds:.0} s, {} encoder.",
        match summary {
            Some(s) if !s.hardware_encoder => "software",
            _ => "hardware",
        }
    );
    Ok(())
}

/// A status line.
fn report(elapsed: Duration, stats: &Stats) {
    let network = match (stats.round_trip, stats.lost) {
        (Some(round_trip), Some(lost)) => format!(
            "; {} ms there and back, {lost:.2}% of packets lost",
            round_trip.as_millis()
        ),
        _ => String::new(),
    };
    println!(
        "{:>4} s: {}, {:.0} pictures/s, {:.0} sound frames/s, {:.1} Mbit/s{network}",
        elapsed.as_secs(),
        stats.quality.name(),
        stats.pictures_per_second,
        stats.sound_per_second,
        stats.megabits_per_second,
    );
}

/// How the sound and the pictures are to go: `--codec` and `--video`.
fn codecs(args: &mut Args) -> Result<(SoundCodec, VideoCodec)> {
    let sound = match args.option("--codec")?.as_deref() {
        None | Some("opus") => SoundCodec::Opus,
        Some("pcm") => SoundCodec::Pcm,
        Some("aac") => SoundCodec::Aac,
        Some(other) => anyhow::bail!("--codec takes opus, pcm or aac, not {other}"),
    };
    let video = match args.option("--video")?.as_deref() {
        None | Some("av1") => VideoCodec::Av1,
        Some("hevc") => VideoCodec::Hevc,
        Some("h264") => VideoCodec::H264,
        Some(other) => anyhow::bail!("--video takes av1, hevc or h264, not {other}"),
    };
    Ok((sound, video))
}

/// Keys pressed in the terminal, as they are pressed.
fn keys() -> std::sync::mpsc::Receiver<char> {
    unsafe extern "C" {
        /// The C runtime's: the next key, without waiting for Enter.
        fn _getch() -> i32;
    }
    let (sender, keys) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        loop {
            // SAFETY: _getch takes nothing and returns a key code.
            let key = unsafe { _getch() };
            if key < 0 || sender.send(char::from(key as u8)).is_err() {
                break;
            }
        }
    });
    keys
}

/// The meeting point to stream or watch through, and how to connect:
/// `--meet URL` (Shuttercrab's unless given), and `--direct` to connect
/// without the relay.
struct Meet {
    url: String,
    path: shuttercrab_stream::Path,
}

impl Meet {
    fn from_args(args: &mut Args) -> Result<Self> {
        Ok(Self {
            url: args
                .option("--meet")?
                .unwrap_or_else(|| shuttercrab_stream::MEET.into()),
            path: if args.flag("--direct") {
                shuttercrab_stream::Path::Direct
            } else {
                shuttercrab_stream::Path::Relay
            },
        })
    }
}

/// The key that opens a room on the meeting point: `SHUTTERCRAB_HOST_KEY`,
/// or `meet/.env` when run from the repository. Never printed.
fn host_key() -> Result<String> {
    if let Ok(key) = std::env::var("SHUTTERCRAB_HOST_KEY") {
        return Ok(key);
    }
    std::fs::read_to_string("meet/.env")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("HOST_KEY="))
                .map(|key| key.trim().to_string())
        })
        .context("no host key: set SHUTTERCRAB_HOST_KEY, or run from the repository, whose meet/.env holds it")
}

/// What the stream's sound is, for the first line.
fn describe_sound(process: Option<u32>, speakers: bool, microphone: bool) -> String {
    match (process, speakers, microphone) {
        (Some(pid), _, true) => format!("process {pid}'s sound and the microphone"),
        (Some(pid), _, false) => format!("process {pid}'s sound"),
        (None, true, true) => "the speakers' sound and the microphone".into(),
        (None, true, false) => "the speakers' sound".into(),
        (None, false, true) => "the microphone".into(),
        (None, false, false) => "no sound".into(),
    }
}

pub(super) fn watch(mut args: Args) -> Result<()> {
    let from = args
        .option("--from")?
        .unwrap_or_else(|| format!("127.0.0.1:{DEFAULT_PORT}"));
    let link = args.option("--link")?;
    let meet = Meet::from_args(&mut args)?;
    let monitor = args.option("--monitor")?;
    let muted = args.option("--sound")?.as_deref() == Some("off");
    args.finish()?;
    let _ = log::set_logger(&Log);
    log::set_max_level(log::LevelFilter::Info);

    let input = match link {
        Some(link) => {
            println!("Joining the stream...");
            Input::online(&Link::parse(&link)?, &meet.url, meet.path)?
        }
        None => Input::local(&from)?,
    };
    let monitors = display::enumerate()?;
    let target = display::select(&monitors, monitor.as_deref())?;
    println!(
        "Escape closes the window; M mutes; [ and ] move the pictures earlier or later against \
         the sound."
    );
    shuttercrab_stream::watch(
        input,
        &WatchOptions {
            monitor: Some(target.hmonitor),
            muted,
            title: "Shuttercrab live (spike)".into(),
        },
    )
}
