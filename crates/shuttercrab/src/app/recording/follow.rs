//! Following a recorded window: its border and the bar move with it, and
//! what waits before its recording starts is known, to cancel if it closes.

use super::Border;
use crate::{app::State, recording, ui::countdown::Countdown, ui::record_bar::RecordBar};
use gpui_kit::Entity;
use shuttercrab_capture::PhysicalRect;
use shuttercrab_platform::{frame::NO_EDGE, window as platform_window};
use shuttercrab_types::WindowId;
use std::cell::{Cell, RefCell};

/// A recorded window, from when it is chosen until its recording ends:
/// Windows reports its moves, and its border and the bar follow.
pub(in crate::app) struct Followed {
    window: WindowId,
    border: Border,
    /// The bar, once it is on screen.
    pub(super) bar: Cell<Option<FollowingBar>>,
    /// What waits for the user before recording starts, to cancel if the
    /// window is closed meanwhile.
    pub(super) waiting: RefCell<Option<Waiting>>,
}

/// What waits for the user before a window's recording starts.
pub(super) enum Waiting {
    Ready(Entity<RecordBar>),
    Countdown(Entity<Countdown>),
}

/// Say what waits for the user now, if a window is followed.
pub(super) fn waiting_on(state: &State, waiting: Option<Waiting>) {
    if let Some(followed) = state.followed.borrow().as_ref() {
        followed.waiting.replace(waiting);
    }
}

/// The bar, as it follows a recorded window.
#[derive(Clone, Copy, Debug)]
pub(super) struct FollowingBar {
    pub(super) os_window: WindowId,
    /// Its size and its distance from the window, physical pixels.
    pub(super) size: (u32, u32),
    pub(super) gap: u32,
}

impl Followed {
    /// Put the border and the bar around the window at `bounds`: the bar
    /// below it, or above it, as when it was first shown.
    pub(super) fn follow(&self, bounds: PhysicalRect) {
        if let Some(frame) = self.border.borrow_mut().as_mut()
            && let Err(e) = frame.place(bounds, NO_EDGE)
        {
            log::warn!("could not move the recording border: {e:#}");
        }
        let Some(bar) = self.bar.get() else {
            return;
        };
        let work =
            platform_window::work_area(platform_window::monitor_of(self.window)).unwrap_or(bounds);
        let (rect, _) = recording::controls_rect(work, bounds, bar.size, bar.gap);
        if let Err(e) = platform_window::cover(bar.os_window, rect) {
            log::warn!("could not move the recording controls: {e:#}");
        }
    }
}

/// Following a chosen window. Dropped before its recording starts (it was
/// cancelled, or could not start), it stops following; once the recording
/// has started, the recording's end stops it.
pub(in crate::app) struct Following<'a> {
    state: &'a State,
    kept: bool,
}

impl<'a> Following<'a> {
    pub(in crate::app) fn start(state: &'a State, window: WindowId, border: Border) -> Self {
        state.followed.replace(Some(Followed {
            window,
            border,
            bar: Cell::new(None),
            waiting: RefCell::new(None),
        }));
        state.platform.follow_window(Some(window));
        Self { state, kept: false }
    }

    /// The recording started: follow until it ends.
    pub(in crate::app) fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for Following<'_> {
    fn drop(&mut self) {
        if !self.kept {
            stop_following(self.state);
        }
    }
}

/// Stop following the recorded window, if one is followed.
pub(in crate::app) fn stop_following(state: &State) {
    if state.followed.take().is_some() {
        state.platform.follow_window(None);
    }
}
