//! Screenshots: the Capture Bar, freezing the monitors, and the selection
//! overlay that chooses what to capture (also used to choose an area to
//! record).

use super::{Busy, State, delivery::deliver, failure::Failure, recording::record};
use crate::{
    capture_bar::{BAR_HEIGHT, BAR_WIDTH, CaptureBar, CaptureBarEvent, CaptureMode, CaptureTarget},
    overlay::{Mode, OverlayEvent, OverlayFrame, SelectionOverlay},
    popup::{self, Activation},
    selection::ScreenWindow,
};
use chrono::Local;
use futures::{
    FutureExt as _, Stream, StreamExt as _, channel::mpsc::UnboundedReceiver, stream::SelectAll,
};
use gpui_kit::{AppContext as _, AsyncApp};
use shuttercrab_capture::{
    CaptureError, FrozenFrame, MonitorId, MonitorInfo, PhysicalRect, Screenshot, cut, cut_shape,
};
use shuttercrab_platform::{targets, window as platform_window};
use std::{cell::Cell, rc::Rc, sync::Arc, time::Instant};

/// The Capture Bar's distance from the top of the monitor, logical pixels.
const BAR_TOP: f32 = 24.0;

/// Where the Capture Bar goes: centred near the top of `monitor`.
pub fn bar_rect(monitor: &MonitorInfo) -> PhysicalRect {
    let (b, scale) = (monitor.bounds, monitor.scale_factor);
    let width = (BAR_WIDTH * scale).round() as u32;
    let height = (BAR_HEIGHT * scale).round() as u32;
    PhysicalRect::new(
        b.x + (b.width.saturating_sub(width) / 2) as i32,
        b.y + (BAR_TOP * scale).round() as i32,
        width,
        height,
    )
}

pub(super) async fn capture_bar(
    state: &Rc<State>,
    monitor: MonitorId,
    pressed: Instant,
    cx: &mut AsyncApp,
) -> Result<(), Failure> {
    let info = monitor_info(state, monitor).await?;
    let (mode, target) = {
        let settings = state.settings.borrow();
        (settings.last_mode, settings.last_target)
    };
    let (bar, outcome) = popup::open(
        &info,
        bar_rect(&info),
        Activation::Take,
        cx,
        move |window, cx| cx.new(|cx| CaptureBar::new(target, window, cx).with_mode(mode)),
    )
    .map_err(|e| Failure::new("Could not open the Capture Bar.", e))?;
    if let Some(hwnd) = bar.hwnd() {
        platform_window::round_corners(hwnd);
        // Never part of a screenshot, even if the screen is frozen while
        // the bar is still fading out.
        exclude_from_capture(hwnd, "Capture Bar");
    }
    log::info!(
        "Capture Bar up {} ms after the request",
        pressed.elapsed().as_millis()
    );
    let mut outcome = outcome;
    let event = outcome.next().await.unwrap_or(CaptureBarEvent::Dismissed);
    bar.close(cx);
    match event {
        CaptureBarEvent::Dismissed => {
            log::info!("Capture Bar dismissed");
            Ok(())
        }
        CaptureBarEvent::Chosen(mode, target) => {
            log::info!("Capture Bar: {mode:?} {target:?}");
            state.update_settings(
                |s| {
                    s.last_mode = mode;
                    s.last_target = target;
                },
                cx,
            );
            match mode {
                CaptureMode::Screenshot => {
                    capture(state, target, monitor, Instant::now(), cx).await
                }
                CaptureMode::Record => record(state, target, monitor, Instant::now(), cx).await,
            }
        }
    }
}

/// Keep one of Shuttercrab's windows out of screenshots and recordings (PRD
/// §13.6). SHUTTERCRAB_CAPTURABLE_UI leaves them capturable, for screenshots
/// of Shuttercrab itself. Returns whether Windows confirmed the exclusion (or
/// it was left out on purpose).
pub(super) fn exclude_from_capture(hwnd: isize, what: &str) -> bool {
    if std::env::var_os("SHUTTERCRAB_CAPTURABLE_UI").is_some() {
        return true;
    }
    match platform_window::exclude_from_capture(hwnd) {
        Ok(()) => true,
        Err(e) => {
            log::warn!("could not exclude the {what} from capture: {e:#}");
            false
        }
    }
}

/// What Shuttercrab knows of `monitor`.
pub(super) async fn monitor_info(
    state: &State,
    monitor: MonitorId,
) -> Result<MonitorInfo, Failure> {
    state
        .capture
        .list_monitors()
        .await?
        .into_iter()
        .find(|m| m.id == monitor)
        .ok_or_else(|| Failure::plain("The monitor under the pointer is gone."))
}

/// Capture `target` on `monitor`.
pub(super) async fn capture(
    state: &Rc<State>,
    target: CaptureTarget,
    monitor: MonitorId,
    pressed: Instant,
    cx: &mut AsyncApp,
) -> Result<(), Failure> {
    let taken_at = Local::now().naive_local();
    let include_cursor = state.settings.borrow().include_cursor;
    let frame = state
        .capture
        .freeze_monitor(monitor, include_cursor)
        .await?;
    let mode = match target {
        CaptureTarget::Display => {
            state.busy.set(Busy::Finishing);
            let info = frame.monitor().clone();
            let (width, height) = frame.size();
            let whole = PhysicalRect::new(0, 0, width, height);
            // Cut, then the frozen screen goes with the task (PRD §23).
            let shot = cx
                .background_spawn(async move { cut(frame.bgra(), width, height, whole) })
                .await;
            return deliver(state, shot?, &info, pressed, taken_at, cx).await;
        }
        CaptureTarget::Area => Mode::Area,
        CaptureTarget::Window => Mode::Window,
        CaptureTarget::Freeform => Mode::Freeform,
    };
    let (frozen, event) = select(state, frame, include_cursor, mode, false, pressed, cx).await?;
    let released = Instant::now();
    // The overlay is gone: a new request can wait for this one.
    state.busy.set(Busy::Finishing);
    let whole = PhysicalRect::new(0, 0, frozen.frame.width, frozen.frame.height);
    let shot = match event {
        OverlayEvent::Cancelled | OverlayEvent::ModeChanged(_) => {
            log::info!("selection cancelled");
            return Ok(());
        }
        OverlayEvent::Selected(rect) => frozen.cut(Cut::Region(rect), cx).await,
        OverlayEvent::Shape(outline) => frozen.cut(Cut::Shape(outline), cx).await,
        OverlayEvent::Display => frozen.cut(Cut::Region(whole), cx).await,
        OverlayEvent::Window { hwnd, visible } => {
            let include_cursor = state.settings.borrow().include_cursor;
            match state.capture.capture_window(hwnd, include_cursor).await {
                Ok(shot) => Ok(shot),
                // Some windows refuse direct capture; what the user saw of
                // the window is the next best thing.
                Err(e) => {
                    log::warn!("direct window capture failed, cutting it from the screen: {e}");
                    frozen.cut(Cut::Region(visible), cx).await
                }
            }
        }
    };
    let info = frozen.info.clone();
    // Cut: the frozen screen can go before the copying and saving (PRD §23).
    drop(frozen);
    deliver(state, shot?, &info, released, taken_at, cx).await
}

/// A frozen monitor, as its overlay shows it. The overlay's image is the
/// screen's only copy; the screenshot is cut from it.
pub(super) struct Frozen {
    pub(super) info: MonitorInfo,
    frame: OverlayFrame,
}

/// What to cut out of a frozen monitor.
enum Cut {
    Region(PhysicalRect),
    Shape(Arc<[(f32, f32)]>),
}

impl Frozen {
    /// Take `frame`'s pixels for the overlay, without a copy.
    fn new(frame: FrozenFrame) -> Self {
        let info = frame.monitor().clone();
        let (width, height) = frame.size();
        let frame = OverlayFrame::from_bgra(width, height, info.scale_factor, frame.into_bgra());
        Self { info, frame }
    }

    /// Cut `what` out and encode it, off the UI thread.
    async fn cut(&self, what: Cut, cx: &AsyncApp) -> Result<Screenshot, CaptureError> {
        let frame = self.frame.clone();
        cx.background_spawn(async move {
            let (bgra, width, height) = (frame.bgra(), frame.width, frame.height);
            match what {
                Cut::Region(region) => cut(bgra, width, height, region),
                Cut::Shape(outline) => cut_shape(bgra, width, height, &outline),
            }
        })
        .await
    }
}

/// Show the selection overlay on every monitor, in `mode` or, with
/// `recording`, to choose an area to record, and wait for the choice.
/// `first` is the monitor under the pointer, frozen: its overlay comes up
/// first and takes the keyboard, then the other monitors are frozen (with
/// the pointer as `include_cursor` says) and covered. Returns the monitor
/// the choice was made on, and the choice; the other frozen screens are
/// released.
pub(super) async fn select(
    state: &State,
    first: FrozenFrame,
    include_cursor: bool,
    mode: Mode,
    recording: bool,
    pressed: Instant,
    cx: &mut AsyncApp,
) -> Result<(Frozen, OverlayEvent), Failure> {
    let frozen = pressed.elapsed();
    let mode = Rc::new(Cell::new(mode));
    let snap = state.settings.borrow().snap_to_windows;
    let open = |frame: &Frozen, activation, cx: &mut AsyncApp| {
        open_overlay(frame, mode.clone(), recording, snap, activation, cx)
    };
    let first = Frozen::new(first);
    let (popup, outcome) = open(&first, Activation::Take, cx)?;
    log::info!(
        "overlay up {} ms after the request (freeze {} ms)",
        pressed.elapsed().as_millis(),
        frozen.as_millis()
    );
    let first_id = first.info.id;
    let mut popups = vec![popup];
    let mut frames = vec![first];
    let mut events = SelectAll::new();
    events.push(tagged(0, outcome));

    let others = state.capture.list_monitors().await.unwrap_or_else(|e| {
        log::warn!("could not list the other monitors: {e}");
        Vec::new()
    });
    let mut chosen = None;
    for monitor in others.into_iter().filter(|m| m.id != first_id) {
        chosen = settled(&mut events, &popups, cx);
        if chosen.is_some() {
            break;
        }
        let frame = match state
            .capture
            .freeze_monitor(monitor.id, include_cursor)
            .await
        {
            Ok(frame) => Frozen::new(frame),
            Err(e) => {
                log::warn!("could not freeze {}: {e}", monitor.device_name);
                continue;
            }
        };
        match open(&frame, Activation::OnClick, cx) {
            Ok((popup, outcome)) => {
                events.push(tagged(frames.len(), outcome));
                popups.push(popup);
                frames.push(frame);
            }
            Err(failure) => log::warn!("{}", failure.detail),
        }
    }
    if popups.len() > 1 {
        log::info!(
            "overlays on {} monitors {} ms after the request",
            popups.len(),
            pressed.elapsed().as_millis()
        );
    }
    let chosen = match chosen.or_else(|| settled(&mut events, &popups, cx)) {
        Some(chosen) => chosen,
        None => loop {
            match events.next().await {
                Some((_, OverlayEvent::ModeChanged(_))) => refresh_all(&popups, cx),
                Some(chosen) => break chosen,
                None => break (0, OverlayEvent::Cancelled),
            }
        },
    };
    for popup in popups {
        popup.close(cx);
    }
    let (index, event) = chosen;
    Ok((frames.swap_remove(index), event))
}

/// Open the selection overlay over `frame`, sharing `mode` with the other
/// monitors' overlays.
fn open_overlay(
    frozen: &Frozen,
    mode: Rc<Cell<Mode>>,
    recording: bool,
    snap: bool,
    activation: Activation,
    cx: &mut AsyncApp,
) -> Result<(popup::Popup, UnboundedReceiver<OverlayEvent>), Failure> {
    let info = frozen.info.clone();
    let windows = screen_windows(info.bounds);
    // Shared, not copied: the frozen screen exists once.
    let overlay_frame = frozen.frame.clone();
    let (overlay, outcome) = popup::open(&info, info.bounds, activation, cx, move |window, cx| {
        cx.new(|cx| {
            let overlay = SelectionOverlay::new(overlay_frame, window, cx)
                .with_windows(windows, snap)
                .sharing_mode(mode);
            if recording {
                overlay.for_recording()
            } else {
                overlay
            }
        })
    })
    .map_err(|e| Failure::new("Could not open the selection screen.", e))?;
    // A screenshot's own screen is frozen before the overlay appears, but a
    // recording may be running (PRD §13.5), or about to start as it goes.
    if let Some(hwnd) = overlay.hwnd() {
        exclude_from_capture(hwnd, "selection overlay");
    }
    Ok((overlay, outcome))
}

/// One overlay's events, with the overlay's index.
fn tagged(
    index: usize,
    events: UnboundedReceiver<OverlayEvent>,
) -> impl Stream<Item = (usize, OverlayEvent)> + Unpin {
    events.map(move |event| (index, event))
}

/// The choice, if one of the overlays has made it already. A mode change
/// on the way redraws them all.
fn settled<S>(
    events: &mut SelectAll<S>,
    popups: &[popup::Popup],
    cx: &mut AsyncApp,
) -> Option<(usize, OverlayEvent)>
where
    S: Stream<Item = (usize, OverlayEvent)> + Unpin,
{
    while let Some(Some(event)) = events.next().now_or_never() {
        match event {
            (_, OverlayEvent::ModeChanged(_)) => refresh_all(popups, cx),
            chosen => return Some(chosen),
        }
    }
    None
}

fn refresh_all(popups: &[popup::Popup], cx: &mut AsyncApp) {
    for popup in popups {
        popup.refresh(cx);
    }
}

/// Windows on the monitor at `bounds`, front to back, relative to it. The
/// desktop is left out: over it, Window mode captures the whole display.
fn screen_windows(bounds: PhysicalRect) -> Vec<ScreenWindow> {
    let monitor = targets::Bounds {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
    };
    targets::visible_windows()
        .into_iter()
        .filter(|w| !w.desktop && w.bounds.intersect(&monitor).is_some())
        .map(|w| ScreenWindow {
            hwnd: w.hwnd,
            bounds: PhysicalRect::new(
                w.bounds.x - bounds.x,
                w.bounds.y - bounds.y,
                w.bounds.width,
                w.bounds.height,
            ),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capture_bar_sits_centred_near_the_top() {
        let monitor = MonitorInfo {
            id: MonitorId(1),
            device_name: String::new(),
            name: String::new(),
            bounds: PhysicalRect::new(3840, 0, 3840, 2160),
            scale_factor: 1.5,
            advanced_color_enabled: true,
            hdr_enabled: true,
            sdr_white_level_nits: Some(240.0),
            adapter: String::new(),
        };
        let rect = bar_rect(&monitor);
        assert_eq!((rect.width, rect.height), (612, 198));
        assert_eq!((rect.x, rect.y), (3840 + (3840 - 612) / 2, 36));
    }
}
