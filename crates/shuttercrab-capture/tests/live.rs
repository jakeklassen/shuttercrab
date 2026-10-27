//! A live recording played as it is recorded (issue #66, spike S0): part
//! of the first monitor, frames straight from the recorder into the live
//! player, muted, with no window.
//!
//!   cargo test -p shuttercrab-capture --test live -- --ignored --nocapture
#![cfg(windows)]

use futures::executor::block_on;
use shuttercrab_capture::{
    Capture, PhysicalRect,
    live::{LiveFormat, LiveFrame, LivePlayer, Track},
    record::{RecordOptions, Recorder},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};

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
}

fn median(mut values: Vec<Duration>) -> Duration {
    values.sort();
    values.get(values.len() / 2).copied().unwrap_or_default()
}

/// `width` × `height` of the first monitor's top left at `fps`, with what
/// the speakers play.
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
        sound_process: None,
        microphone: false,
        microphone_device: None,
        sound_track: true,
        path: std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("unused.mp4"),
    }
}

#[test]
#[ignore = "records the screen and plays it: run by hand"]
fn a_live_recording_plays_as_it_is_recorded() {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let (width, height, fps) = (1280, 720, 60);
    let options = options(width, height, fps);
    let format = LiveFormat {
        width,
        height,
        fps,
        sound: true,
    };
    let shown = Arc::new(AtomicU32::new(0));
    let counted = shown.clone();
    let failures = Arc::new(Mutex::new(Vec::new()));
    let failed = failures.clone();
    let player = Arc::new(
        LivePlayer::new(
            format,
            move |why| failed.lock().unwrap().push(why),
            move || {
                counted.fetch_add(1, Ordering::Relaxed);
            },
        )
        .unwrap(),
    );
    player.set_muted(true).unwrap();

    let arrived = Arc::new(Mutex::new(Arrived::default()));
    let started = Instant::now();
    let (sink_player, sink_arrived) = (player.clone(), arrived.clone());
    let recorder = Recorder::start_live(
        options,
        Arc::new(move |frame: LiveFrame| {
            {
                let mut arrived = sink_arrived.lock().unwrap();
                arrived.bytes += frame.data.len();
                match frame.track {
                    Track::Video => {
                        arrived.video += 1;
                        arrived.keys += u32::from(frame.key);
                        let captured = Duration::from_nanos(frame.time.max(0) as u64 * 100);
                        arrived
                            .delays
                            .push(started.elapsed().saturating_sub(captured));
                    }
                    Track::Sound => arrived.sound += 1,
                }
            }
            if let Err(e) = sink_player.push(frame) {
                eprintln!("push: {e:#}");
            }
        }),
    )
    .unwrap();

    let mut behind = Vec::new();
    for _ in 0..24 {
        std::thread::sleep(Duration::from_millis(250));
        behind.push(player.behind());
    }
    let summary = recorder.stop().unwrap();
    let arrived = arrived.lock().unwrap();
    let seconds = summary.duration.as_secs_f64();
    println!("{summary:?}");
    println!(
        "{} pictures ({} key), {} sound frames, {:.1} Mbit/s; player showed {} pictures",
        arrived.video,
        arrived.keys,
        arrived.sound,
        arrived.bytes as f64 * 8.0 / seconds / 1e6,
        shown.load(Ordering::Relaxed)
    );
    let sender = median(arrived.delays.clone());
    let player_behind = median(behind[8..].to_vec());
    println!(
        "capture to sink, median {} ms; player behind the newest, median {} ms: about {} ms in all, before the network",
        sender.as_millis(),
        player_behind.as_millis(),
        (sender + player_behind).as_millis()
    );
    println!(
        "player behind, every 250 ms: {:?}",
        behind.iter().map(|d| d.as_millis()).collect::<Vec<_>>()
    );
    let failures = failures.lock().unwrap();
    assert!(failures.is_empty(), "{failures:?}");
    assert!(arrived.video > 0 && arrived.sound > 0);
    // A still screen sends a picture a second, so there may be only the
    // first key frame.
    assert!(arrived.keys >= 1);
    assert!(
        shown.load(Ordering::Relaxed) > 0,
        "the player showed nothing"
    );
}
