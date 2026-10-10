//! What of a monitor a recording takes: sizes the encoders accept.

use crate::screen::PhysicalRect;

/// The smallest recording, physical pixels a side. Media Foundation's
/// software H.264 encoder refuses 32×32 and takes 34×34; smaller than this
/// is no use as a video anyway.
pub const MIN_SIDE: u32 = 64;

/// Hardware encoders have minimum sizes of their own (NVENC's H.264 needs
/// at least 145×49, and fails only once frames arrive), so smaller
/// recordings use the software encoder, which is quick at such sizes.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows backend records yet")
)]
pub(crate) const HARDWARE_MIN_SIDE: u32 = 256;

/// What of a `width`×`height` monitor is recorded for `region`: all of it
/// for `None`; otherwise the region on the monitor, grown about its centre
/// to [`MIN_SIDE`] if smaller, with even sizes, as NV12 requires.
pub fn recordable(region: Option<PhysicalRect>, width: u32, height: u32) -> PhysicalRect {
    let full = PhysicalRect::new(0, 0, width, height);
    let r = region.unwrap_or(full).clamp_to(width, height);
    let grow = |at: i32, side: u32, limit: u32| {
        if side >= MIN_SIDE || limit < MIN_SIDE {
            return (at, side.min(limit));
        }
        let start = at - (MIN_SIDE - side) as i32 / 2;
        (start.clamp(0, (limit - MIN_SIDE) as i32), MIN_SIDE)
    };
    let (x, w) = grow(r.x, r.width, width);
    let (y, h) = grow(r.y, r.height, height);
    PhysicalRect::new(x, y, w & !1, h & !1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_regions_are_even_on_screen_and_not_too_small() {
        let rect = |x, y, w, h| Some(PhysicalRect::new(x, y, w, h));
        // All of it, or the region with odd sizes made even.
        assert_eq!(
            recordable(None, 3840, 2160),
            PhysicalRect::new(0, 0, 3840, 2160)
        );
        assert_eq!(
            recordable(rect(10, 20, 1001, 777), 3840, 2160),
            PhysicalRect::new(10, 20, 1000, 776)
        );
        // Off the edge: cut to the monitor (40×60 here), then grown back
        // to the minimum inside it.
        assert_eq!(
            recordable(rect(3800, 2100, 400, 400), 3840, 2160),
            PhysicalRect::new(3776, 2096, 64, 64)
        );
        // Tiny: grown about its centre, kept on the monitor.
        assert_eq!(
            recordable(rect(100, 300, 20, 200), 3840, 2160),
            PhysicalRect::new(78, 300, 64, 200)
        );
        assert_eq!(
            recordable(rect(0, 0, 10, 10), 3840, 2160),
            PhysicalRect::new(0, 0, 64, 64)
        );
    }
}
