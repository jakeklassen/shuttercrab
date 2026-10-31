//! What the window shows for a moment, such as Copy's check mark or a
//! notice over the screenshot. Shown again before its time is up, it stays
//! its full time from then.

use super::MainWindow;
use crate::generation::{Generation, Token};
use gpui_kit::Context;
use std::time::Duration;

/// Something shown for a moment, if it is now.
pub(super) struct Momentary<T> {
    /// What is shown, and which showing it is.
    shown: Option<(T, Token)>,
    /// Counts showings, so the timer of one leaves a later one be.
    showings: Generation,
}

impl<T> Default for Momentary<T> {
    fn default() -> Self {
        Self {
            shown: None,
            showings: Generation::default(),
        }
    }
}

impl<T> Momentary<T> {
    /// What is shown, if anything.
    pub(super) fn get(&self) -> Option<&T> {
        self.shown.as_ref().map(|(value, _)| value)
    }

    pub(super) fn is_shown(&self) -> bool {
        self.shown.is_some()
    }

    /// Which showing is on, if any: a new one each time it is shown.
    pub(super) fn showing(&self) -> Option<Token> {
        self.shown.as_ref().map(|(_, showing)| *showing)
    }
}

impl MainWindow {
    /// Show `value` in the [`Momentary`] that `slot` picks, for `duration`.
    pub(super) fn show_for_a_moment<T: 'static>(
        &mut self,
        slot: fn(&mut Self) -> &mut Momentary<T>,
        value: T,
        duration: Duration,
        cx: &mut Context<Self>,
    ) {
        let momentary = slot(self);
        let showing = momentary.showings.start();
        momentary.shown = Some((value, showing));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(duration).await;
            // Unless it was shown again meanwhile, for a time of its own.
            let _ = this.update(cx, |this, cx| {
                let momentary = slot(this);
                if momentary.showings.is_current(showing) {
                    momentary.shown = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
}
