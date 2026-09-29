//! Recording helpers shared by the app and its recording UI (PRD §7.7, §16).

use std::time::Duration;

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
}
