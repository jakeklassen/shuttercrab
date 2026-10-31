//! Windows' clock for pictures and sound: QueryPerformanceCounter in 100 ns
//! ticks. Windows.Graphics.Capture stamps frames with it, the speakers
//! report their position in it, and Media Foundation times samples in the
//! same units.

pub(crate) use crate::record::TICKS_PER_SECOND;
use std::time::Duration;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// Now, in ticks.
pub(crate) fn now() -> i64 {
    let (mut counter, mut frequency) = (0i64, 0i64);
    unsafe {
        let _ = QueryPerformanceCounter(&mut counter);
        let _ = QueryPerformanceFrequency(&mut frequency);
    }
    (counter as i128 * TICKS_PER_SECOND as i128 / frequency.max(1) as i128) as i64
}

/// A `Duration` as ticks.
pub(crate) fn ticks(duration: Duration) -> i64 {
    (duration.as_nanos() / 100) as i64
}

/// Ticks as a `Duration`, none if negative.
pub(crate) fn duration(ticks: i64) -> Duration {
    Duration::from_nanos(ticks.max(0) as u64 * 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_are_100_ns() {
        assert_eq!(ticks(Duration::from_millis(1)), 10_000);
        assert_eq!(duration(10_000), Duration::from_millis(1));
        assert_eq!(duration(-5), Duration::ZERO);
        let (a, b) = (now(), now());
        assert!(a > 0 && b >= a);
    }
}
