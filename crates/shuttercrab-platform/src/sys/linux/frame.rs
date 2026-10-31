//! The recording border under X11: one override-redirect window per strip,
//! filled with the colour and cut by the Shape extension to just its
//! dashes, so the gaps show what is underneath and the pointer passes
//! through. The window manager leaves such windows alone: they take no
//! focus and are in no task list.
//!
//! X11 cannot keep a window out of captures, so a side that would lie
//! inside the area (where it meets its monitor's edge) is not shown.

use super::window::{X11, x11};
use crate::frame::{FrameStyle, strips};
use anyhow::{Context, Result, anyhow, ensure};
use shuttercrab_types::PhysicalRect;
use shuttercrab_types::WindowId;
use x11rb::{
    COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT,
    connection::Connection,
    protocol::{
        shape::{self, ConnectionExt as _},
        xproto::{
            ChangeWindowAttributesAux, ClipOrdering, ConfigureWindowAux, ConnectionExt as _,
            CreateWindowAux, Rectangle, StackMode, Window, WindowClass,
        },
    },
};

/// The border on screen. Dropping it removes it.
pub struct Frame {
    strips: Vec<(PhysicalRect, Window)>,
    style: FrameStyle,
}

impl Frame {
    /// Show a border of `color` (RGB) around `area` on a monitor with
    /// `bounds`. Returns it and how many sides could not be shown.
    pub fn show(
        area: PhysicalRect,
        bounds: PhysicalRect,
        style: FrameStyle,
        color: [u8; 3],
    ) -> Result<(Self, usize)> {
        let x = x11().ok_or_else(|| anyhow!("the recording border needs X11"))?;
        let capturable = std::env::var_os("SHUTTERCRAB_CAPTURABLE_UI").is_some();
        let pixel = pixel(x, color);
        let mut frame = Self {
            strips: Vec::new(),
            style,
        };
        let mut missing = 0;
        for (strip, covers) in strips(area, bounds, style.thickness) {
            if covers && !capturable {
                // It would be in the recording.
                missing += 1;
                continue;
            }
            let window = x.conn.generate_id()?;
            x.conn
                .create_window(
                    COPY_DEPTH_FROM_PARENT,
                    window,
                    x.root,
                    strip.x as i16,
                    strip.y as i16,
                    strip.width as u16,
                    strip.height as u16,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    COPY_FROM_PARENT,
                    &CreateWindowAux::new()
                        .override_redirect(1)
                        .background_pixel(pixel),
                )?
                .check()
                .context("making the recording border")?;
            // Kept before the window shows, so nothing else is ever drawn.
            frame.strips.push((strip, window));
            shape_dashes(x, window, strip, style)?;
            // Clicks go to whatever is under the border.
            x.conn.shape_rectangles(
                shape::SO::SET,
                shape::SK::INPUT,
                ClipOrdering::UNSORTED,
                window,
                0,
                0,
                &[],
            )?;
            x.conn.map_window(window)?;
        }
        x.conn.flush()?;
        Ok((frame, missing))
    }

    /// The strips' windows, for platform calls and tests.
    pub fn windows(&self) -> Vec<WindowId> {
        self.strips
            .iter()
            .map(|&(_, w)| WindowId::from_raw(u64::from(w)))
            .collect()
    }

    /// Redraw the border in `color` (RGB).
    pub fn recolor(&mut self, color: [u8; 3]) -> Result<()> {
        let x = x11().ok_or_else(|| anyhow!("the recording border needs X11"))?;
        let pixel = pixel(x, color);
        for &(_, window) in &self.strips {
            x.conn.change_window_attributes(
                window,
                &ChangeWindowAttributesAux::new().background_pixel(pixel),
            )?;
            x.conn.clear_area(true, window, 0, 0, 0, 0)?;
        }
        x.conn.flush()?;
        Ok(())
    }

    /// Move the border to `area` on a monitor with `bounds`. Fails if the
    /// sides would change where they go (inside or outside), so the caller
    /// shows a new border instead.
    pub fn place(&mut self, area: PhysicalRect, bounds: PhysicalRect) -> Result<()> {
        let x = x11().ok_or_else(|| anyhow!("the recording border needs X11"))?;
        let placed = strips(area, bounds, self.style.thickness);
        ensure!(
            placed.len() == self.strips.len() && placed.iter().all(|(_, covers)| !covers),
            "the border's sides would change"
        );
        for ((strip, window), (new, _)) in self.strips.iter_mut().zip(placed) {
            x.conn.configure_window(
                *window,
                &ConfigureWindowAux::new()
                    .x(new.x)
                    .y(new.y)
                    .width(new.width)
                    .height(new.height)
                    .stack_mode(StackMode::ABOVE),
            )?;
            // The dashes follow desktop coordinates, so they move too.
            shape_dashes(x, *window, new, self.style)?;
            *strip = new;
        }
        x.conn.flush()?;
        Ok(())
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        if let Some(x) = x11() {
            for &(_, window) in &self.strips {
                let _ = x.conn.destroy_window(window);
            }
            let _ = x.conn.flush();
        }
    }
}

/// Cut `window`, which shows `strip`, to its dashes.
fn shape_dashes(x: &X11, window: Window, strip: PhysicalRect, style: FrameStyle) -> Result<()> {
    x.conn.shape_rectangles(
        shape::SO::SET,
        shape::SK::BOUNDING,
        ClipOrdering::UNSORTED,
        window,
        0,
        0,
        &dash_rectangles(strip, style),
    )?;
    Ok(())
}

/// The dashes of `strip`, in its own coordinates. As in
/// [`crate::frame::dashes`], the pattern follows desktop coordinates, so the
/// strips' dashes line up at the corners.
fn dash_rectangles(strip: PhysicalRect, style: FrameStyle) -> Vec<Rectangle> {
    let period = (style.dash + style.gap).max(1) as i32;
    let horizontal = strip.width >= strip.height;
    let (start, length) = if horizontal {
        (strip.x, strip.width as i32)
    } else {
        (strip.y, strip.height as i32)
    };
    let mut rectangles = Vec::new();
    // From the dash that covers the strip's start, or the first after it.
    let mut along = start - start.rem_euclid(period);
    while along < start + length {
        let from = along.max(start);
        let to = (along + style.dash as i32).min(start + length);
        if to > from {
            let (offset, run) = ((from - start) as i16, (to - from) as u16);
            rectangles.push(if horizontal {
                Rectangle {
                    x: offset,
                    y: 0,
                    width: run,
                    height: strip.height as u16,
                }
            } else {
                Rectangle {
                    x: 0,
                    y: offset,
                    width: strip.width as u16,
                    height: run,
                }
            });
        }
        along += period;
    }
    rectangles
}

/// `color` (RGB) as a pixel value of the screen's visual: true colour,
/// with each channel where its mask says.
fn pixel(x: &X11, [red, green, blue]: [u8; 3]) -> u32 {
    let screen = x.conn.setup().roots.iter().find(|s| s.root == x.root);
    let visual = screen.and_then(|s| {
        s.allowed_depths
            .iter()
            .flat_map(|d| &d.visuals)
            .find(|v| v.visual_id == s.root_visual)
    });
    let Some(v) = visual else {
        return u32::from(red) << 16 | u32::from(green) << 8 | u32::from(blue);
    };
    let place = |value: u8, mask: u32| {
        let bits = mask.count_ones().min(8);
        (u32::from(value) >> (8 - bits)) << mask.trailing_zeros()
    };
    place(red, v.red_mask) | place(green, v.green_mask) | place(blue, v.blue_mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLE: FrameStyle = FrameStyle {
        thickness: 2,
        dash: 3,
        gap: 2,
    };

    #[test]
    fn dashes_follow_the_desktop_like_the_windows_pattern() {
        // Starting mid-period (x = 4: one pixel into the gap before 5).
        let got = dash_rectangles(PhysicalRect::new(4, 0, 10, 2), STYLE);
        let runs: Vec<(i16, u16)> = got.iter().map(|r| (r.x, r.width)).collect();
        // Dashes at 5..8, 10..13 (and 15.. is past the end at 14).
        assert_eq!(runs, [(1, 3), (6, 3)]);
        assert!(got.iter().all(|r| r.y == 0 && r.height == 2));
        // The same pixels the painted pattern colours.
        let painted = crate::frame::dashes(PhysicalRect::new(4, 0, 10, 1), STYLE, [1, 2, 3]);
        let on: Vec<i16> = painted
            .chunks(4)
            .enumerate()
            .filter(|(_, p)| p[3] == 255)
            .map(|(i, _)| i as i16)
            .collect();
        assert_eq!(on, [1, 2, 3, 6, 7, 8]);
    }

    #[test]
    fn a_vertical_strip_is_cut_along_its_height() {
        let got = dash_rectangles(PhysicalRect::new(0, 1, 2, 6), STYLE);
        let runs: Vec<(i16, u16)> = got.iter().map(|r| (r.y, r.height)).collect();
        // Dashes at 0..3 (from 1), 5..8 (to 7).
        assert_eq!(runs, [(0, 2), (4, 2)]);
        assert!(got.iter().all(|r| r.x == 0 && r.width == 2));
    }
}
