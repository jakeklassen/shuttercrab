//! The recorded window changing: moving and being dragged, going out of
//! sight and coming back, and closing.

use super::{follow::Waiting, show};
use crate::{app::State, generation::Token};
use gpui_kit::AsyncApp;
use shuttercrab_platform::WindowChange;
use std::{rc::Rc, time::Duration};

/// The recorded window changed: its border and the bar follow it, the
/// pointer is left out while it is dragged, and minimising it pauses the
/// recording until it is restored.
pub(in crate::app) fn window_changed(state: &Rc<State>, change: WindowChange, cx: &mut AsyncApp) {
    match change {
        WindowChange::Moved(bounds) => {
            if let Some(followed) = state.followed.borrow().as_ref() {
                followed.follow(bounds);
            }
        }
        // Windows draws the pointer out of step with a window being
        // dragged, so it hops about in the recording.
        WindowChange::DragStarted | WindowChange::DragEnded => {
            if let Some(recorder) = state
                .recording
                .borrow()
                .as_ref()
                .and_then(|r| r.recorder.as_ref())
            {
                recorder.show_cursor(change == WindowChange::DragEnded);
            }
        }
        WindowChange::Minimized => out_of_sight(state, "the window is minimised", cx),
        WindowChange::Hidden => out_of_sight(state, "the window is hidden", cx),
        WindowChange::Restored => back_in_sight(state, cx),
        WindowChange::Closed => window_closed(state, cx),
    }
}

/// The recorded window was closed: the recording ends, as when Windows
/// closes the capture itself. Before it starts, the ready bar or the
/// countdown is cancelled, as Escape would.
fn window_closed(state: &State, cx: &mut AsyncApp) {
    if let Some(recording) = state.recording.borrow().as_ref() {
        log::info!("the recorded window was closed");
        if let Some(recorder) = &recording.recorder {
            recorder.window_closed();
        }
        return;
    }
    let waiting = state
        .followed
        .borrow()
        .as_ref()
        .and_then(|f| f.waiting.take());
    match waiting {
        Some(Waiting::Ready(bar)) => {
            log::info!("the window to record was closed; the ready bar closes");
            bar.update(cx, |bar, cx| bar.close(cx));
        }
        Some(Waiting::Countdown(countdown)) => {
            log::info!("the window to record was closed; the countdown stops");
            countdown.update(cx, |countdown, cx| countdown.cancel(cx));
        }
        None => {}
    }
}

/// How long a restored window takes to draw itself again. Windows hands
/// over junk frames (a solid colour) while it animates back from the
/// taskbar, so the recording resumes only after this.
const RESTORE_SETTLE: Duration = Duration::from_millis(300);

/// Pause while the recorded window is out of sight (`why` says how):
/// Windows draws nothing of it meanwhile.
fn out_of_sight(state: &State, why: &'static str, cx: &mut AsyncApp) {
    let view = {
        let mut slot = state.recording.borrow_mut();
        let Some(recording) = slot.as_mut() else {
            return;
        };
        // A resume still waiting for an earlier restore does not happen.
        recording.hidings.start();
        // A question or a discard has paused it already.
        if recording.asking.is_some() || recording.discarded.is_some() {
            return;
        }
        // Minimised, then hidden, say: still paused, for the new reason.
        if !recording.paused_out_of_sight {
            if !recording.set_paused(true) {
                return;
            }
            recording.paused_out_of_sight = true;
        }
        log::info!("{why}; recording paused");
        recording.view()
    };
    show(view, cx, |bar, cx| bar.pause_reason(Some(why.into()), cx));
    state.refresh_tray_menu();
}

/// The recorded window is shown again: resume once it has drawn itself
/// again, unless the recording was paused by hand before.
fn back_in_sight(state: &Rc<State>, cx: &mut AsyncApp) {
    let hiding = match state.recording.borrow().as_ref() {
        Some(r) if r.paused_out_of_sight => r.hidings.current(),
        _ => return,
    };
    let state = state.clone();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(RESTORE_SETTLE).await;
        resume_restored(&state, hiding, cx);
    })
    .detach();
}

/// Resume after the window was restored, if it did not go out of sight
/// again since (`hiding` is still current) and that still has it paused.
fn resume_restored(state: &State, hiding: Token, cx: &mut AsyncApp) {
    let view = {
        let mut slot = state.recording.borrow_mut();
        let Some(recording) = slot.as_mut() else {
            return;
        };
        if !recording.hidings.is_current(hiding)
            || recording.asking.is_some()
            || recording.discarded.is_some()
            || !std::mem::take(&mut recording.paused_out_of_sight)
        {
            return;
        }
        recording.set_paused(false);
        log::info!("the window is back; recording resumed");
        recording.view()
    };
    show(view, cx, |bar, cx| bar.pause_reason(None, cx));
    state.refresh_tray_menu();
}
