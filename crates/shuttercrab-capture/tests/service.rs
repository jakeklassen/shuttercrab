//! The capture service against the real desktop.
//!
//! `lists_monitors` only reads display state. The freeze test captures the
//! screen (in memory, nothing is saved), so it is ignored by default:
//!
//!   cargo test -p shuttercrab-capture --test service -- --ignored
#![cfg(windows)]

use futures::executor::block_on;
use shuttercrab_capture::{Capture, PhysicalRect, monitor_under_pointer};
use std::time::Instant;

#[test]
fn lists_monitors() {
    let capture = Capture::start().unwrap();
    let monitors = block_on(capture.list_monitors()).unwrap();
    assert!(!monitors.is_empty());
    for m in &monitors {
        assert!(m.bounds.width > 0 && m.bounds.height > 0, "{m:?}");
        assert!(m.scale_factor >= 1.0, "{m:?}");
        assert!(!m.hdr_enabled || m.advanced_color_enabled, "{m:?}");
    }
    let under = monitor_under_pointer().unwrap();
    assert!(monitors.iter().any(|m| m.id == under));
}

#[test]
#[ignore = "captures the screen"]
fn freezes_and_cuts_a_screenshot() {
    let capture = Capture::start().unwrap();
    capture.warm_up();
    let monitor = monitor_under_pointer().unwrap();
    for attempt in 0..2 {
        let started = Instant::now();
        let frame = block_on(capture.freeze_monitor(monitor, false)).unwrap();
        let froze = started.elapsed();
        let (w, h) = frame.size();
        assert_eq!(
            (w, h),
            (frame.monitor().bounds.width, frame.monitor().bounds.height)
        );
        assert_eq!(frame.preview_bgra().len(), (w * h * 4) as usize);
        let region = PhysicalRect::new(10, 20, 640, 360);
        let started = Instant::now();
        let shot = block_on(capture.screenshot(&frame, region)).unwrap();
        println!(
            "attempt {attempt}: froze {}x{} in {:?} (peak {:.2}, {} HDR regions); cut + PNG in {:?}, {} bytes",
            w,
            h,
            froze,
            frame.frame_peak(),
            frame.hdr_regions(),
            started.elapsed(),
            shot.png.len()
        );
        assert_eq!((shot.width, shot.height), (640, 360));
        // The screenshot is exactly the preview's pixels.
        let bgra = frame.preview_bgra();
        let i = ((20 * w + 10) * 4) as usize;
        assert_eq!(&shot.rgba[..3], [bgra[i + 2], bgra[i + 1], bgra[i]]);
        assert!(block_on(capture.screenshot(&frame, PhysicalRect::new(0, 0, w + 1, 1))).is_err());
        drop(frame);
    }
}
