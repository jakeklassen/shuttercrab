//! The dashed border around what is recorded: its colours and look, and
//! showing it.

use shuttercrab_capture::{MonitorInfo, PhysicalRect};
use shuttercrab_platform::frame::{Frame, FrameStyle, NO_EDGE};
use shuttercrab_types::WindowId;
use std::{cell::RefCell, rc::Rc};

/// The dashed border, shared with what moves it after a recorded window.
pub(in crate::app) type Border = Rc<RefCell<Option<Frame>>>;

/// The border's colours (RGB): recording, paused, and discarded.
pub(in crate::app) const BORDER_RECORDING: [u8; 3] = [0xE5, 0x48, 0x4D];
pub(super) const BORDER_PAUSED: [u8; 3] = [0xF5, 0xA5, 0x24];
pub(super) const BORDER_DISCARDED: [u8; 3] = [0x9D, 0x9D, 0x9D];
/// Before recording starts, during a countdown.
pub(in crate::app) const BORDER_WAITING: [u8; 3] = BORDER_DISCARDED;

/// The border's look, logical pixels.
const BORDER_THICKNESS: f32 = 2.0;
const BORDER_DASH: f32 = 8.0;
const BORDER_GAP: f32 = 5.0;

/// Show the dashed border around `region` (physical pixels relative to the
/// monitor; `None` for all of it), or around `window`, in `color`, before
/// recording starts, so it is excluded from the first frame.
pub(in crate::app) fn show_border(
    info: &MonitorInfo,
    region: Option<PhysicalRect>,
    window: Option<WindowId>,
    color: [u8; 3],
) -> Option<Frame> {
    let (b, scale) = (info.bounds, info.scale_factor);
    let area = shuttercrab_capture::record::recordable(region, b.width, b.height);
    let px = |logical: f32| ((logical * scale).round() as u32).max(1);
    let style = FrameStyle {
        thickness: px(BORDER_THICKNESS),
        dash: px(BORDER_DASH),
        gap: px(BORDER_GAP),
    };
    let area = PhysicalRect::new(b.x + area.x, b.y + area.y, area.width, area.height);
    let bounds = b;
    // A window's own picture is recorded, never the border: it goes
    // outside the window wherever the window is.
    let (area, bounds) = match window {
        Some(window) => (
            shuttercrab_platform::visible_bounds(window).unwrap_or(area),
            NO_EDGE,
        ),
        None => (area, bounds),
    };
    match Frame::show(area, bounds, style, color) {
        Ok((frame, 0)) => Some(frame),
        Ok((frame, missing)) => {
            log::warn!("{missing} sides of the recording border would be recorded; left out");
            Some(frame)
        }
        Err(e) => {
            log::warn!("could not show the recording border: {e:#}");
            None
        }
    }
}
