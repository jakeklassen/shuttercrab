//! Watching a stream (issue #66): the Watch box, filled in from the
//! clipboard when a link is there, and the viewer it starts in a process of
//! its own.

use super::{
    State,
    failure::Failure,
    screenshot::{exclude_from_capture, monitor_info},
};
use crate::{
    popup::{self, Activation},
    watch_box::{WATCH_BOX_HEIGHT, WATCH_BOX_WIDTH, WatchBox, WatchBoxEvent},
};
use futures::StreamExt as _;
use gpui_kit::{AppContext as _, AsyncApp};
use shuttercrab_capture::{MonitorId, PhysicalRect};
use shuttercrab_platform::window as platform_window;
use std::rc::Rc;

/// Ask for a stream's link on `monitor`, near the top like the Capture Bar,
/// and open the viewer for it.
pub(super) async fn open_watch_box(
    state: &Rc<State>,
    monitor: MonitorId,
    cx: &mut AsyncApp,
) -> Result<(), Failure> {
    let info = monitor_info(state, monitor).await?;
    let on_clipboard = cx
        .update(|cx| cx.read_from_clipboard())
        .and_then(|item| item.text())
        .filter(|text| crate::watch::is_link(text))
        .map(|text| text.trim().to_string());
    let (b, scale) = (info.bounds, info.scale_factor);
    let (w, h) = (
        (WATCH_BOX_WIDTH * scale).round() as u32,
        (WATCH_BOX_HEIGHT * scale).round() as u32,
    );
    let rect = PhysicalRect::new(
        b.x + (b.width as i32 - w as i32) / 2,
        b.y + (b.height as f32 * 0.2) as i32,
        w,
        h,
    );
    let (popup, mut events) = popup::open(&info, rect, Activation::Take, cx, move |window, cx| {
        cx.new(|cx| WatchBox::new(on_clipboard, window, cx))
    })
    .map_err(|e| Failure::new("Could not open the Watch box.", e))?;
    if let Some(os_window) = popup.os_window() {
        platform_window::round_corners(os_window);
        exclude_from_capture(os_window, "Watch box");
    }
    let event = events.next().await.unwrap_or(WatchBoxEvent::Cancel);
    popup.close(cx);
    match event {
        WatchBoxEvent::Watch(link) => {
            log::info!("watching a stream");
            crate::watch::open(&link)
                .map_err(|e| Failure::new("Could not open the viewer.", format!("{e:#}")))
        }
        WatchBoxEvent::Cancel => Ok(()),
    }
}
