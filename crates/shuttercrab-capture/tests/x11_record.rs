//! Recording against a real X server: a second of a monitor's region, then
//! the MP4 read back by GStreamer. It opens no windows and presses no
//! keys, but it runs with the other X11 tests, on their private server:
//!
//!   scripts/linux-x11-tests.sh
#![cfg(target_os = "linux")]

use futures::executor::block_on;
use gstreamer::{self as gst, prelude::*};
use shuttercrab_capture::{
    Capture, PhysicalRect,
    record::{RecordOptions, Recorder, missing_software},
};
use std::{path::PathBuf, time::Duration};

#[test]
#[ignore = "needs a private X server: scripts/linux-x11-tests.sh"]
fn records_a_region_to_an_mp4_that_plays() {
    assert!(
        std::env::var_os("SHUTTERCRAB_TEST_X11").is_some(),
        "run scripts/linux-x11-tests.sh"
    );
    assert_eq!(missing_software(), None);
    let monitors = block_on(Capture::start().unwrap().list_monitors()).unwrap();
    let monitor = monitors.first().expect("a monitor");
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("record");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("region.mp4");
    let options = RecordOptions {
        monitor: monitor.id,
        // Odd sizes are rounded down to even ones.
        region: Some(PhysicalRect::new(10, 20, 641, 361)),
        window: None,
        fps: 30,
        include_cursor: true,
        system_sound: false,
        sound_process: None,
        microphone: false,
        microphone_device: None,
        sound_track: false,
        path: path.clone(),
    };
    let mut recorder = Recorder::start(options).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    recorder.pause();
    std::thread::sleep(Duration::from_millis(300));
    recorder.resume();
    std::thread::sleep(Duration::from_millis(500));
    let ended = recorder.ended().expect("the first call");
    let summary = recorder.stop().unwrap();
    block_on(ended);

    assert_eq!((summary.width, summary.height), (640, 360));
    assert!(summary.interrupted.is_none(), "{summary:?}");
    // The pause starts and ends when the recording thread hears of it,
    // a moment after each call.
    assert!(summary.paused >= Duration::from_millis(250), "{summary:?}");
    let duration = summary.duration.as_secs_f64();
    assert!((0.8..1.5).contains(&duration), "{summary:?}");
    assert!(summary.frames >= 20, "{summary:?}");

    // GStreamer reads it back: an H.264 track of the recorded length.
    let (frames, length) = read_back(&path);
    assert!(
        frames as u64 + 2 >= summary.frames,
        "{frames} of {summary:?}"
    );
    assert!((length.as_secs_f64() - duration).abs() < 0.2, "{length:?}");
    std::fs::remove_dir_all(&folder).unwrap();
}

/// The video frames in the MP4 at `path`, and its duration.
fn read_back(path: &std::path::Path) -> (usize, Duration) {
    let pipeline = gst::parse::launch(&format!(
        "filesrc location=\"{}\" ! qtdemux ! video/x-h264 ! appsink name=frames sync=false",
        path.display()
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let sink = pipeline
        .by_name("frames")
        .unwrap()
        .downcast::<gstreamer_app::AppSink>()
        .unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();
    let mut frames = 0;
    while sink.pull_sample().is_ok() {
        frames += 1;
    }
    let length = pipeline
        .query_duration::<gst::ClockTime>()
        .map_or(Duration::ZERO, |d| Duration::from_nanos(d.nseconds()));
    pipeline.set_state(gst::State::Null).unwrap();
    (frames, length)
}
