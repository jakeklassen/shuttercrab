//! Reading the recorded area under X11: the root window's pixels, as the
//! compositor shows them, with the pointer drawn in from XFixes (X11
//! leaves it out of what `GetImage` reads).

use super::super::service::{X11, check_format};
use crate::{record::RecordOptions, screen::PhysicalRect};
use anyhow::{Context, Result, anyhow};
use x11rb::{
    connection::Connection,
    protocol::{
        xfixes::ConnectionExt as _,
        xproto::{ConnectionExt as _, ImageFormat},
    },
};

/// The recorded area and a connection to read it with.
pub(super) struct Grab {
    x: X11,
    /// Root window coordinates, even sizes.
    area: PhysicalRect,
    /// Whether XFixes can give the pointer's image.
    pointer: bool,
}

impl Grab {
    /// The area `options` ask for: the region of the monitor, or all of it,
    /// rounded down to even sizes (4:2:0 video needs them).
    pub(super) fn new(options: &RecordOptions) -> Result<Self> {
        let x = X11::connect()?;
        let monitor = x
            .monitors()?
            .into_iter()
            .find(|m| m.id == options.monitor)
            .ok_or_else(|| anyhow!("the monitor is no longer attached"))?;
        let b = monitor.bounds;
        let area = match options.region {
            Some(r) => PhysicalRect::new(b.x + r.x, b.y + r.y, r.width, r.height),
            None => b,
        };
        let area = PhysicalRect::new(area.x, area.y, area.width & !1, area.height & !1);
        if area.width == 0 || area.height == 0 {
            return Err(anyhow!("nothing to record in {area:?}"));
        }
        let pointer = x
            .conn
            .xfixes_query_version(5, 0)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some();
        if !pointer {
            log::warn!("no XFixes: the pointer is left out of the recording");
        }
        Ok(Self { x, area, pointer })
    }

    pub(super) fn size(&self) -> (u32, u32) {
        (self.area.width, self.area.height)
    }

    /// The area now, as BGRx, with the pointer drawn in if `pointer`.
    pub(super) fn frame(&self, pointer: bool) -> Result<Vec<u8>> {
        let a = self.area;
        let image = self
            .x
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.x.root,
                a.x as i16,
                a.y as i16,
                a.width as u16,
                a.height as u16,
                !0,
            )?
            .reply()
            .context("reading the screen")?;
        check_format(&image.data, image.depth, self.x.conn.setup(), self.size())?;
        let mut frame = image.data;
        if pointer && self.pointer {
            self.draw_pointer(&mut frame);
        }
        Ok(frame)
    }

    /// Draw the pointer's image where it is. It comes premultiplied, so
    /// each pixel is its colour plus what shows through.
    fn draw_pointer(&self, frame: &mut [u8]) {
        let Some(cursor) = self
            .x
            .conn
            .xfixes_get_cursor_image()
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            return;
        };
        let left = i32::from(cursor.x) - i32::from(cursor.xhot) - self.area.x;
        let top = i32::from(cursor.y) - i32::from(cursor.yhot) - self.area.y;
        draw_premultiplied(
            frame,
            self.size(),
            &cursor.cursor_image,
            (u32::from(cursor.width), u32::from(cursor.height)),
            (left, top),
        );
    }
}

/// Draw `image`, premultiplied ARGB of `size`, onto `frame`, BGRx of
/// `frame_size`, with its top left at `at`. What falls outside is left out.
fn draw_premultiplied(
    frame: &mut [u8],
    (frame_width, frame_height): (u32, u32),
    image: &[u32],
    (width, height): (u32, u32),
    (left, top): (i32, i32),
) {
    for row in 0..height {
        let y = top + row as i32;
        if y < 0 || y >= frame_height as i32 {
            continue;
        }
        for column in 0..width {
            let x = left + column as i32;
            if x < 0 || x >= frame_width as i32 {
                continue;
            }
            let Some(&argb) = image.get((row * width + column) as usize) else {
                return;
            };
            let [b, g, r, a] = argb.to_le_bytes();
            if a == 0 {
                continue;
            }
            let at = (y as usize * frame_width as usize + x as usize) * 4;
            let through = u32::from(255 - a);
            for (channel, colour) in frame[at..at + 3].iter_mut().zip([b, g, r]) {
                *channel = (u32::from(colour) + (u32::from(*channel) * through + 127) / 255)
                    .min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pointer_is_drawn_over_the_frame() {
        // A 4×2 grey frame, and a 2×1 pointer: opaque red, then half-
        // transparent white (premultiplied, so 128 of each).
        let mut frame = vec![100u8; 4 * 2 * 4];
        let image = [0xFFFF_0000, 0x8080_8080];
        draw_premultiplied(&mut frame, (4, 2), &image, (2, 1), (1, 1));
        let pixel = |x: usize, y: usize| &frame[(y * 4 + x) * 4..(y * 4 + x) * 4 + 3];
        assert_eq!(pixel(0, 0), [100, 100, 100]);
        assert_eq!(pixel(1, 1), [0, 0, 255]);
        // 128 + 100 × 127/255.
        assert_eq!(pixel(2, 1), [178, 178, 178]);
        assert_eq!(pixel(3, 1), [100, 100, 100]);
    }

    #[test]
    fn a_pointer_partly_off_the_frame_is_clipped() {
        let mut frame = vec![0u8; 2 * 2 * 4];
        let image = [0xFFFF_FFFF; 9];
        draw_premultiplied(&mut frame, (2, 2), &image, (3, 3), (-2, -2));
        assert_eq!(&frame[..4], [255, 255, 255, 0]);
        assert!(frame[4..].iter().all(|&c| c == 0));
    }
}
