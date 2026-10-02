//! Recording helpers shared by the app and the recording controls (PRD
//! §7.7, §16): the elapsed-time clock and where the controls go.

use shuttercrab_capture::PhysicalRect;
use std::time::{Duration, Instant};

/// A recording's length as a clock: `0:07`, `12:34`, `1:02:03`.
pub fn clock(length: Duration) -> String {
    let seconds = length.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// How long a recording has run, paused time left out, as the output
/// timeline counts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    started: Instant,
    /// Paused time before the current pause.
    paused_for: Duration,
    /// When the current pause began.
    paused_since: Option<Instant>,
}

impl Clock {
    pub fn new(now: Instant) -> Self {
        Self {
            started: now,
            paused_for: Duration::ZERO,
            paused_since: None,
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused_since.is_some()
    }

    pub fn pause(&mut self, now: Instant) {
        self.paused_since.get_or_insert(now);
    }

    pub fn resume(&mut self, now: Instant) {
        if let Some(since) = self.paused_since.take() {
            self.paused_for += now.saturating_duration_since(since);
        }
    }

    /// The recorded length at `now`.
    pub fn elapsed(&self, now: Instant) -> Duration {
        let end = self.paused_since.unwrap_or(now);
        end.saturating_duration_since(self.started)
            .saturating_sub(self.paused_for)
    }
}

/// Where the recording controls go, physical pixels: centred below the
/// recorded `region`, or above it, within the monitor's `work` area; if
/// neither fits (a display recording), inside the region along its
/// bottom. Returns the rectangle and whether it covers part of the region,
/// which is only safe while the controls are excluded from capture.
pub fn controls_rect(
    work: PhysicalRect,
    region: PhysicalRect,
    size: (u32, u32),
    gap: u32,
) -> (PhysicalRect, bool) {
    let (w, h) = (size.0 as i32, size.1 as i32);
    let gap = gap as i32;
    let (work_right, work_bottom) = (work.x + work.width as i32, work.y + work.height as i32);
    let region_bottom = region.y + region.height as i32;
    let centred = region.x + (region.width as i32 - w) / 2;
    let x = centred.clamp(work.x, (work_right - w).max(work.x));
    let fits = |y: i32| y >= work.y && y + h <= work_bottom;
    let below = region_bottom + gap;
    let above = region.y - gap - h;
    let y = if fits(below) {
        below
    } else if fits(above) {
        above
    } else {
        (region_bottom.min(work_bottom) - gap - h).max(work.y)
    };
    let rect = PhysicalRect::new(x, y, size.0, size.1);
    (rect, overlaps(rect, region))
}

fn overlaps(a: PhysicalRect, b: PhysicalRect) -> bool {
    a.x < b.x + b.width as i32
        && b.x < a.x + a.width as i32
        && a.y < b.y + b.height as i32
        && b.y < a.y + a.height as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_read_like_a_player() {
        let at = |s: f64| clock(Duration::from_secs_f64(s));
        assert_eq!(at(0.0), "0:00");
        assert_eq!(at(7.9), "0:07");
        assert_eq!(at(754.0), "12:34");
        assert_eq!(at(3723.0), "1:02:03");
    }

    #[test]
    fn paused_time_is_left_out_and_stands_still() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut c = Clock::new(t0);
        assert_eq!(c.elapsed(t0 + s(10)), s(10));
        c.pause(t0 + s(10));
        assert!(c.is_paused());
        assert_eq!(c.elapsed(t0 + s(25)), s(10));
        // Pausing twice keeps the first pause.
        c.pause(t0 + s(20));
        c.resume(t0 + s(30));
        assert!(!c.is_paused());
        assert_eq!(c.elapsed(t0 + s(35)), s(15));
        c.resume(t0 + s(40));
        assert_eq!(c.elapsed(t0 + s(40)), s(20));
    }

    #[test]
    fn controls_sit_outside_an_area_and_inside_a_display() {
        // A 3840×2160 monitor with a 72-pixel taskbar; controls 540×72.
        let work = PhysicalRect::new(0, 0, 3840, 2088);
        let size = (540, 72);
        // Room below: centred under the area.
        let (rect, covers) = controls_rect(work, PhysicalRect::new(1000, 200, 1200, 800), size, 18);
        assert_eq!(rect, PhysicalRect::new(1330, 1018, 540, 72));
        assert!(!covers);
        // No room below: above it.
        let (rect, covers) =
            controls_rect(work, PhysicalRect::new(1000, 1200, 1200, 850), size, 18);
        assert_eq!((rect.x, rect.y), (1330, 1110));
        assert!(!covers);
        // Near the left edge: kept on the monitor.
        let (rect, _) = controls_rect(work, PhysicalRect::new(0, 100, 200, 200), size, 18);
        assert_eq!((rect.x, rect.y), (0, 318));
        // The whole display: inside, along the bottom, above the taskbar.
        let (rect, covers) = controls_rect(work, PhysicalRect::new(0, 0, 3840, 2160), size, 18);
        assert_eq!((rect.x, rect.y), (1650, 2088 - 18 - 72));
        assert!(covers);
    }

    #[test]
    fn controls_follow_a_monitor_off_the_origin() {
        let work = PhysicalRect::new(3840, 0, 3840, 2088);
        let (rect, covers) =
            controls_rect(work, PhysicalRect::new(4000, 300, 800, 600), (540, 72), 18);
        assert_eq!((rect.x, rect.y), (4130, 918));
        assert!(!covers);
    }
}
