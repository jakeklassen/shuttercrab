//! The recording border on Windows: one layered window per strip, excluded
//! from capture.

use super::{layered::LayeredWindow, window::exclude_from_capture};
use crate::frame::{FrameStyle, Rect, dashes, strips};
use anyhow::Result;

/// The border on screen. Dropping it removes it; drop it on the thread
/// that showed it.
pub struct Frame {
    strips: Vec<(Rect, LayeredWindow)>,
    style: FrameStyle,
    color: [u8; 3],
}

impl Frame {
    /// Show a border of `color` (RGB) around `area` on a monitor with
    /// `bounds`. Returns it and how many sides could not be shown.
    pub fn show(
        area: Rect,
        bounds: Rect,
        style: FrameStyle,
        color: [u8; 3],
    ) -> Result<(Self, usize)> {
        let capturable = std::env::var_os("SHUTTERCRAB_CAPTURABLE_UI").is_some();
        let mut shown = Vec::new();
        let mut missing = 0;
        for (strip, covers) in strips(area, bounds, style.thickness) {
            let window = LayeredWindow::new()?;
            let excluded = capturable || exclude_from_capture(window.hwnd).is_ok();
            if covers && !excluded {
                // It would be in the recording.
                missing += 1;
                continue;
            }
            window.paint(
                Some((strip.x, strip.y)),
                strip.width,
                strip.height,
                &dashes(strip, style, color),
            )?;
            window.show();
            shown.push((strip, window));
        }
        Ok((
            Self {
                strips: shown,
                style,
                color,
            },
            missing,
        ))
    }

    /// The strips' windows, for platform calls and tests.
    pub fn windows(&self) -> Vec<isize> {
        self.strips.iter().map(|(_, w)| w.hwnd).collect()
    }

    /// Redraw the border in `color` (RGB).
    pub fn recolor(&mut self, color: [u8; 3]) -> Result<()> {
        self.color = color;
        for (strip, window) in &self.strips {
            window.paint(
                None,
                strip.width,
                strip.height,
                &dashes(*strip, self.style, color),
            )?;
        }
        Ok(())
    }

    /// Move the border to `area` on a monitor with `bounds`. Fails if the
    /// sides would change where they go (inside or outside), so the caller
    /// shows a new border instead.
    pub fn place(&mut self, area: Rect, bounds: Rect) -> Result<()> {
        let color = self.color;
        let placed = strips(area, bounds, self.style.thickness);
        anyhow::ensure!(
            placed.len() == self.strips.len() && placed.iter().all(|(_, covers)| !covers),
            "the border's sides would change"
        );
        for ((strip, window), (new, _)) in self.strips.iter_mut().zip(placed) {
            window.paint(
                Some((new.x, new.y)),
                new.width,
                new.height,
                &dashes(new, self.style, color),
            )?;
            *strip = new;
        }
        Ok(())
    }
}
