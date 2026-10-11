//! Recording against a real X server: a second of a monitor's region, then
//! the MP4 read back by GStreamer. It opens no windows and presses no
//! keys, but it runs with the other X11 tests, on their private server:
//!
//!   scripts/linux-x11-tests.sh
//!
//! That script also makes a silent output to play a tone into, when the
//! sound server is PipeWire, for the test that records sound.
#![cfg(target_os = "linux")]

use futures::executor::block_on;
use gstreamer::{self as gst, prelude::*};
use shuttercrab_capture::{
    Capture, PhysicalRect,
    record::{RecordOptions, Recorder, RecordingSummary, microphones, missing_software},
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[test]
#[ignore = "needs a private X server: scripts/linux-x11-tests.sh"]
fn records_a_region_to_an_mp4_that_plays() {
    assert!(
        std::env::var_os("SHUTTERCRAB_TEST_X11").is_some(),
        "run scripts/linux-x11-tests.sh"
    );
    assert_eq!(missing_software(true), None);
    let folder = test_folder("region");
    let path = folder.join("region.mp4");
    let options = RecordOptions {
        // Odd sizes are rounded down to even ones.
        region: Some(PhysicalRect::new(10, 20, 641, 361)),
        // With no sound server (as in CI) the track is silent, but there.
        system_sound: true,
        ..options(&path)
    };
    let recorder = Recorder::start(options).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    recorder.pause();
    std::thread::sleep(Duration::from_millis(300));
    recorder.resume();
    std::thread::sleep(Duration::from_millis(500));
    let summary = stop(recorder);

    assert_eq!((summary.width, summary.height), (640, 360));
    assert!(summary.interrupted.is_none(), "{summary:?}");
    // The pause starts and ends when the recording thread hears of it,
    // a moment after each call.
    assert!(summary.paused >= Duration::from_millis(250), "{summary:?}");
    let duration = summary.duration.as_secs_f64();
    assert!((0.8..1.5).contains(&duration), "{summary:?}");
    assert!(summary.frames >= 20, "{summary:?}");
    assert!(summary.sound, "{summary:?}");

    // GStreamer reads it back: an H.264 track of the recorded length, and
    // an AAC one as long.
    let file = read_back(&path);
    assert!(
        file.frames as u64 + 2 >= summary.frames,
        "{} of {summary:?}",
        file.frames
    );
    assert!(
        (file.length.as_secs_f64() - duration).abs() < 0.2,
        "{:?}",
        file.length
    );
    assert!(
        (file.sound_length().as_secs_f64() - duration).abs() < 0.2,
        "{:?} of sound",
        file.sound_length()
    );
    std::fs::remove_dir_all(&folder).unwrap();
}

#[test]
#[ignore = "needs a sound server and a silent output: scripts/linux-x11-tests.sh"]
fn records_a_microphone_and_switches_it_off() {
    let sink = std::env::var("SHUTTERCRAB_TEST_SOUND_SINK")
        .expect("run scripts/linux-x11-tests.sh, with PipeWire");
    // The script's silent output plays back into a virtual microphone.
    let listed = microphones().unwrap();
    assert!(
        listed.iter().any(|m| m.id == "shuttercrab-test-mic"),
        "{listed:?}"
    );
    // A tone into the silent output, recorded from its monitor as the
    // microphone.
    gst::init().unwrap();
    let tone = gst::parse::launch(&format!(
        "audiotestsrc volume=0.5 is-live=true ! pulsesink device={sink}"
    ))
    .unwrap();
    tone.set_state(gst::State::Playing).unwrap();
    let folder = test_folder("microphone");
    let path = folder.join("microphone.mp4");
    let options = RecordOptions {
        microphone: true,
        microphone_device: Some(format!("{sink}.monitor")),
        ..options(&path)
    };
    let recorder = Recorder::start(options).unwrap();
    std::thread::sleep(Duration::from_millis(1000));
    recorder.set_sound(shuttercrab_capture::record::Source::Microphone, false);
    std::thread::sleep(Duration::from_millis(1000));
    let summary = stop(recorder);
    tone.set_state(gst::State::Null).unwrap();

    assert!(
        summary.sound && summary.interrupted.is_none(),
        "{summary:?}"
    );
    let file = read_back(&path);
    // The tone while the microphone was on (the track runs a fifth of a
    // second behind; well inside that), silence once it was off.
    let on = file.loudness(0.3..0.7);
    let off = file.loudness(1.5..1.9);
    assert!(on > 5_000.0, "{on} while on");
    assert!(off < 10.0, "{off} once off");
    std::fs::remove_dir_all(&folder).unwrap();
}

/// Options for a second of the first monitor at 30 fps, without sound
/// sources, into `path`.
fn options(path: &Path) -> RecordOptions {
    let monitors = block_on(Capture::start().unwrap().list_monitors()).unwrap();
    let monitor = monitors.first().expect("a monitor");
    RecordOptions {
        monitor: monitor.id,
        region: Some(PhysicalRect::new(0, 0, 320, 240)),
        window: None,
        fps: 30,
        include_cursor: true,
        system_sound: false,
        sound_process: None,
        microphone: false,
        microphone_device: None,
        sound_track: true,
        path: path.to_path_buf(),
    }
}

fn test_folder(name: &str) -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn stop(mut recorder: Recorder) -> RecordingSummary {
    let ended = recorder.ended().expect("the first call");
    let summary = recorder.stop().unwrap();
    block_on(ended);
    summary
}

/// What an MP4 holds, as GStreamer reads it.
struct Contents {
    /// Video frames.
    frames: usize,
    length: Duration,
    /// The sound, decoded: 48 kHz, 16-bit, the two channels interleaved.
    sound: Vec<i16>,
}

impl Contents {
    fn sound_length(&self) -> Duration {
        Duration::from_secs_f64(self.sound.len() as f64 / 2.0 / 48_000.0)
    }

    /// The sound's root mean square from `seconds.start` to `seconds.end`.
    fn loudness(&self, seconds: std::ops::Range<f64>) -> f64 {
        let at = |s: f64| ((s * 48_000.0) as usize * 2).min(self.sound.len());
        let part = &self.sound[at(seconds.start)..at(seconds.end)];
        let sum: f64 = part.iter().map(|&s| f64::from(s).powi(2)).sum();
        (sum / part.len().max(1) as f64).sqrt()
    }
}

fn read_back(path: &Path) -> Contents {
    let pipeline = gst::parse::launch(&format!(
        "filesrc location=\"{}\" ! qtdemux name=demux \
         demux.video_0 ! queue ! video/x-h264 ! appsink name=frames sync=false \
         demux.audio_0 ! queue ! decodebin ! audioconvert ! audioresample ! \
         audio/x-raw,format=S16LE,rate=48000,channels=2 ! appsink name=sound sync=false",
        path.display()
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let sink = |name: &str| {
        pipeline
            .by_name(name)
            .unwrap()
            .downcast::<gstreamer_app::AppSink>()
            .unwrap()
    };
    let (frames_sink, sound_sink) = (sink("frames"), sink("sound"));
    pipeline.set_state(gst::State::Playing).unwrap();
    let mut frames = 0;
    while frames_sink.pull_sample().is_ok() {
        frames += 1;
    }
    let mut sound = Vec::new();
    while let Ok(sample) = sound_sink.pull_sample() {
        let buffer = sample.buffer().unwrap().map_readable().unwrap();
        let (pairs, _) = buffer.as_slice().as_chunks::<2>();
        sound.extend(pairs.iter().map(|&bytes| i16::from_le_bytes(bytes)));
    }
    let length = pipeline
        .query_duration::<gst::ClockTime>()
        .map_or(Duration::ZERO, |d| Duration::from_nanos(d.nseconds()));
    pipeline.set_state(gst::State::Null).unwrap();
    Contents {
        frames,
        length,
        sound,
    }
}
