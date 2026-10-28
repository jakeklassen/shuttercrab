//! A live recording played as it is recorded (issue #66, spike S0): part
//! of the first monitor, frames straight from the recorder into the live
//! player, muted, drawn into a texture rather than a window.
//!
//!   cargo test -p shuttercrab-capture --test live -- --ignored --nocapture
#![cfg(windows)]

use futures::executor::block_on;
use shuttercrab_capture::{
    Capture, PhysicalRect,
    gpu::Gpu,
    live::{LiveFormat, LiveFrame, LivePlayer, SoundCodec, Track},
    record::{Live, RecordOptions, Recorder},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use windows::Win32::Graphics::{
    Direct3D11::{D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE},
    Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
};

/// What arrived from the recorder.
#[derive(Default)]
struct Arrived {
    video: u32,
    keys: u32,
    sound: u32,
    bytes: usize,
    /// How long after its capture each picture reached the sink, roughly
    /// (the timeline starts at the first picture, a moment after start).
    delays: Vec<Duration>,
    /// The same for each sound frame, on the same timeline.
    sound_delays: Vec<Duration>,
}

/// The recorder's log, on stderr: it says how late sound arrived.
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

fn median(mut values: Vec<Duration>) -> Duration {
    values.sort();
    values.get(values.len() / 2).copied().unwrap_or_default()
}

/// `width` × `height` of the first monitor's top left at `fps`, with what
/// the speakers play: only process `LIVE_SOUND_OF`'s share, if set; and
/// the microphone if `LIVE_MIC` is set.
fn options(width: u32, height: u32, fps: u32) -> RecordOptions {
    let monitors = block_on(Capture::start().unwrap().list_monitors()).unwrap();
    let monitor = monitors.first().expect("a monitor");
    RecordOptions {
        monitor: monitor.id,
        region: Some(PhysicalRect::new(0, 0, width, height)),
        window: None,
        fps,
        include_cursor: true,
        system_sound: true,
        sound_process: std::env::var("LIVE_SOUND_OF")
            .ok()
            .map(|pid| pid.parse().unwrap()),
        microphone: std::env::var_os("LIVE_MIC").is_some(),
        microphone_device: None,
        sound_track: true,
        path: std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("unused.mp4"),
    }
}

/// Draw each picture as it falls due into a texture, until `done`: how
/// many were drawn. A viewer draws once a refresh; this polls.
fn draw_until(player: &LivePlayer, (width, height): (u32, u32), done: &AtomicBool) -> u32 {
    let device = player.device();
    let context = unsafe { device.GetImmediateContext() }.unwrap();
    let gpu = Gpu::from_device(device, context).unwrap();
    let texture = gpu
        .texture(
            width,
            height,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
            None,
        )
        .unwrap();
    let mut drawn = 0;
    while !done.load(Ordering::Relaxed) {
        if player.draw(&texture).unwrap() {
            drawn += 1;
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    drawn
}

/// Note what arrives in `arrived`, then hand it to `player`.
fn sink(player: Arc<LivePlayer>, arrived: Arc<Mutex<Arrived>>, started: Instant) -> Live {
    Live {
        sink: Arc::new(move |frame: LiveFrame| {
            {
                let mut arrived = arrived.lock().unwrap();
                arrived.bytes += frame.data.len();
                let captured = Duration::from_nanos(frame.time.max(0) as u64 * 100);
                let delay = started.elapsed().saturating_sub(captured);
                match frame.track {
                    Track::Video => {
                        arrived.video += 1;
                        arrived.keys += u32::from(frame.key);
                        arrived.delays.push(delay);
                    }
                    Track::Sound => {
                        arrived.sound += 1;
                        arrived.sound_delays.push(delay);
                    }
                }
            }
            if let Err(e) = player.push(frame) {
                eprintln!("push: {e:#}");
            }
        }),
        sound: SoundCodec::Pcm,
        size: None,
        bitrate: None,
    }
}

#[test]
#[ignore = "records the screen and plays it: run by hand"]
fn a_live_recording_plays_as_it_is_recorded() {
    log::set_logger(&Log).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    let (width, height, fps) = (1280, 720, 60);
    let options = options(width, height, fps);
    let format = LiveFormat {
        width,
        height,
        fps,
        sound: Some(SoundCodec::Pcm),
    };
    let failures = Arc::new(Mutex::new(Vec::new()));
    let failed = failures.clone();
    let player = Arc::new(
        LivePlayer::new(format, move |why| failed.lock().unwrap().push(why))
            .unwrap_or_else(|e| panic!("{e:#}")),
    );
    player.set_muted(true);
    let done = Arc::new(AtomicBool::new(false));
    let drawing = {
        let (player, done) = (player.clone(), done.clone());
        std::thread::spawn(move || draw_until(&player, (width, height), &done))
    };

    let arrived = Arc::new(Mutex::new(Arrived::default()));
    let started = Instant::now();
    let recorder =
        Recorder::start_live(options, sink(player.clone(), arrived.clone(), started)).unwrap();
    let (mut behind, mut sound_behind) = (Vec::new(), Vec::new());
    for _ in 0..24 {
        std::thread::sleep(Duration::from_millis(250));
        behind.push(player.behind());
        sound_behind.extend(player.sound_behind());
    }
    let summary = recorder.stop().unwrap();
    done.store(true, Ordering::Relaxed);
    let drawn = drawing.join().unwrap();

    let arrived = arrived.lock().unwrap();
    let seconds = summary.duration.as_secs_f64();
    println!(
        "{} pictures ({} key), {} sound frames, {:.1} Mbit/s; the player drew {} pictures",
        arrived.video,
        arrived.keys,
        arrived.sound,
        arrived.bytes as f64 * 8.0 / seconds / 1e6,
        drawn
    );
    println!(
        "sound frames reach the sink {} ms later than pictures of the same time (median)",
        median(arrived.sound_delays.clone())
            .saturating_sub(median(arrived.delays.clone()))
            .as_millis()
    );
    let ms = |values: &[Duration]| values.iter().map(|d| d.as_millis()).collect::<Vec<_>>();
    println!(
        "behind the newest picture, every 250 ms: the picture drawn {:?}; the sound {:?}",
        ms(&behind),
        ms(&sound_behind)
    );
    let failures = failures.lock().unwrap();
    assert!(failures.is_empty(), "{failures:?}");
    assert!(arrived.video > 0 && arrived.sound > 0);
    // A still screen sends a picture a second, so there may be only the
    // first key frame.
    assert!(arrived.keys >= 1);
    assert!(drawn > 0, "the player drew nothing");
}

/// Draw into a texture of the player's size, following it as it changes,
/// until `done`: how many pictures were drawn at each size.
fn draw_following(player: &LivePlayer, done: &AtomicBool) -> Vec<((u32, u32), u32)> {
    let device = player.device();
    let context = unsafe { device.GetImmediateContext() }.unwrap();
    let gpu = Gpu::from_device(device, context).unwrap();
    let mut drawn: Vec<((u32, u32), u32)> = Vec::new();
    let mut target = None;
    while !done.load(Ordering::Relaxed) {
        let size = player.size();
        if target.as_ref().is_none_or(|(s, _)| *s != size) {
            let texture = gpu
                .texture(
                    size.0,
                    size.1,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
                    None,
                )
                .unwrap();
            target = Some((size, texture));
        }
        let (size, texture) = target.as_ref().unwrap();
        if player.draw(texture).unwrap() {
            match drawn.last_mut() {
                Some((s, n)) if s == size => *n += 1,
                _ => drawn.push((*size, 1)),
            }
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    drawn
}

#[test]
#[ignore = "records the screen and plays it: run by hand"]
fn the_player_follows_the_stream_to_a_new_picture_size() {
    let fps = 30;
    let mut options = options(1280, 720, fps);
    options.system_sound = false;
    options.sound_track = false;
    let format = LiveFormat {
        width: 1280,
        height: 720,
        fps,
        sound: None,
    };
    let player = Arc::new(LivePlayer::new(format, |_| {}).unwrap_or_else(|e| panic!("{e:#}")));
    let done = Arc::new(AtomicBool::new(false));
    let drawing = {
        let (player, done) = (player.clone(), done.clone());
        std::thread::spawn(move || draw_following(&player, &done))
    };
    // As the spike's sender does: each recorder's timeline moved on to
    // where the stream has got to.
    let started = Instant::now();
    for size in [(1280, 720), (640, 360)] {
        let offset = (started.elapsed().as_nanos() / 100) as i64;
        let to = player.clone();
        let live = Live {
            sink: Arc::new(move |frame: LiveFrame| {
                let frame = LiveFrame {
                    time: frame.time + offset,
                    ..frame
                };
                to.push(frame).unwrap();
            }),
            sound: SoundCodec::Pcm,
            size: Some(size),
            bitrate: None,
        };
        let recorder = Recorder::start_live(options.clone(), live).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        recorder.stop().unwrap();
    }
    std::thread::sleep(Duration::from_millis(300));
    done.store(true, Ordering::Relaxed);
    let drawn = drawing.join().unwrap();
    println!("pictures drawn at each size: {drawn:?}");
    let sizes: Vec<_> = drawn.iter().map(|(s, _)| *s).collect();
    assert_eq!(sizes, [(1280, 720), (640, 360)]);
    // A still screen sends a picture a second; a moving one more.
    assert!(drawn.iter().all(|(_, n)| *n >= 1), "{drawn:?}");
}

/// What a live picture sent smaller looks like, for a person to judge: the
/// first monitor at 1080p, through the encoder and the player, saved as a
/// PNG where `LIVE_SNAPSHOT` says.
#[test]
#[ignore = "records the screen and saves a picture of it: run by hand"]
fn a_live_picture_sent_smaller_can_be_looked_at() {
    let Some(path) = std::env::var_os("LIVE_SNAPSHOT") else {
        return;
    };
    let (fps, size) = (30, (1920, 1080));
    let monitors = block_on(Capture::start().unwrap().list_monitors()).unwrap();
    let bounds = monitors.first().expect("a monitor").bounds;
    let mut options = options(bounds.width, bounds.height, fps);
    options.system_sound = false;
    options.sound_track = false;
    let format = LiveFormat {
        width: size.0,
        height: size.1,
        fps,
        sound: None,
    };
    let player = Arc::new(LivePlayer::new(format, |_| {}).unwrap());
    let to = player.clone();
    let live = Live {
        sink: Arc::new(move |frame: LiveFrame| to.push(frame).unwrap()),
        sound: SoundCodec::Pcm,
        size: Some(size),
        bitrate: Some(10_000_000),
    };
    let recorder = Recorder::start_live(options, live).unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    recorder.stop().unwrap();

    let device = player.device();
    let context = unsafe { device.GetImmediateContext() }.unwrap();
    let gpu = Gpu::from_device(device, context).unwrap();
    let texture = gpu
        .texture(
            size.0,
            size.1,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
            None,
        )
        .unwrap();
    assert!(player.draw(&texture).unwrap(), "nothing to draw");
    let rgba = shuttercrab_capture::premultiplied_bgra_to_rgba(&gpu.read_back(&texture).unwrap());
    let png = shuttercrab_capture::encode_png(size.0, size.1, &rgba).unwrap();
    std::fs::write(&path, png).unwrap();
    println!("saved {}", std::path::Path::new(&path).display());
}
