//! Throwing a take away, and taking that back: asking first, Restart,
//! Discard and Undo, and the take a restart replaced.

use super::{
    Recording, border::BORDER_DISCARDED, finish::discard_recording, show, start_take,
    stop_following, watch_recording,
};
use crate::{
    app::{State, failure::Failure},
    files,
    generation::Token,
    record_bar::{BarMode, Destructive, RecordBar},
    recorder_process::RecorderProcess,
    recording::{self, Clock},
};
use chrono::Local;
use gpui_kit::{AsyncApp, Entity};
use shuttercrab_capture::record::Source;
use shuttercrab_platform::window as platform_window;
use shuttercrab_types::WindowId;
use std::{
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

/// A finished take that a restart replaced: deleted when its undo window
/// closes, unless Undo keeps it.
pub(in crate::app) struct PreviousTake {
    /// Its final name; until kept it stays at [`files::partial_path`].
    path: PathBuf,
    undo_window: Token,
}

/// The controls are asking before a take is thrown away.
#[derive(Clone, Copy, Debug)]
pub(super) struct Asking {
    action: Destructive,
    /// The recording was running when asked, and resumes if kept.
    resume: bool,
    /// The window that had the keyboard before the bar took it to ask.
    give_back: Option<WindowId>,
}

/// A discard that can still be undone: the recording is paused meanwhile.
#[derive(Clone, Copy, Debug)]
pub(in crate::app) struct Discarded {
    /// The recording was running, and resumes if the discard is undone.
    resume: bool,
    undo_window: Token,
    /// The window that had the keyboard before the controls took it.
    pub(super) give_back: Option<WindowId>,
}

/// Discard or Restart was asked for: ask first, or act at once and offer
/// undo, as the settings say. Asking for the same action again while the
/// controls ask confirms it.
pub(in crate::app) async fn request(state: &Rc<State>, action: Destructive, cx: &mut AsyncApp) {
    let (asking, discarded) = match &*state.recording.borrow() {
        Some(r) => (r.asking.map(|a| a.action), r.discarded.is_some()),
        None => return,
    };
    match asking {
        Some(asked) if asked == action => return confirm(state, cx).await,
        Some(_) => return,
        None => {}
    }
    // Discarding a discarded take again removes it at once; restarting it
    // means nothing.
    if discarded {
        if action == Destructive::Discard {
            confirm(state, cx).await;
        }
        return;
    }
    // A restart's previous take is still on offer.
    if action == Destructive::Restart && state.previous_take.borrow().is_some() {
        return;
    }
    let confirm_first = state.settings.borrow().confirm_discard;
    match (confirm_first, action) {
        (true, _) => ask(state, action, cx),
        (false, Destructive::Discard) => discard_with_undo(state, cx),
        (false, Destructive::Restart) => restart_recording(state, true, cx).await,
    }
}

/// Pause and ask before `action` throws the take away. The controls take
/// the keyboard so Enter or Escape answers; it goes back afterwards.
fn ask(state: &State, action: Destructive, cx: &mut AsyncApp) {
    let (view, os_window, length) = {
        let mut slot = state.recording.borrow_mut();
        let Some(recording) = slot.as_mut() else {
            return;
        };
        if recording.recorder.is_none() {
            return;
        }
        let resume = recording.set_paused(true);
        let os_window = recording
            .controls
            .as_ref()
            .and_then(|c| c.popup.os_window());
        let give_back = platform_window::foreground_window().filter(|w| Some(*w) != os_window);
        recording.asking = Some(Asking {
            action,
            resume,
            give_back,
        });
        let length = recording.clock.get().elapsed(Instant::now());
        (recording.view(), os_window, length)
    };
    log::info!(
        "asking before {action:?} of a {} take",
        recording::clock(length)
    );
    show(view, cx, |bar, cx| {
        bar.set_mode(BarMode::Confirm(action, length), cx)
    });
    if let Some(os_window) = os_window {
        platform_window::bring_to_front(os_window);
    }
    state.refresh_tray_menu();
}

/// Stop asking: the controls show the actions again and the keyboard goes
/// back to where it was. Returns what was being asked.
pub(super) fn stop_asking(state: &State, cx: &mut AsyncApp) -> Option<Asking> {
    let (asking, view) = {
        let mut slot = state.recording.borrow_mut();
        let recording = slot.as_mut()?;
        (recording.asking.take()?, recording.view())
    };
    show(view, cx, |bar, cx| bar.set_mode(BarMode::Controls, cx));
    if let Some(window) = asking.give_back {
        platform_window::bring_to_front(window);
    }
    Some(asking)
}

/// Yes: throw the take away as asked, or a discarded one at once rather
/// than when its undo window closes.
pub(super) async fn confirm(state: &Rc<State>, cx: &mut AsyncApp) {
    let Some(asking) = stop_asking(state, cx) else {
        let discarded = state
            .recording
            .borrow()
            .as_ref()
            .is_some_and(|r| r.discarded.is_some());
        if discarded {
            discard_recording(state, cx);
        }
        return;
    };
    match asking.action {
        Destructive::Discard => discard_recording(state, cx),
        Destructive::Restart => restart_recording(state, false, cx).await,
    }
}

/// No: keep the take, and carry on recording if it was running.
pub(super) fn keep_take(state: &State, cx: &mut AsyncApp) {
    let Some(asking) = stop_asking(state, cx) else {
        return;
    };
    let view = {
        let recording = state.recording.borrow();
        let Some(recording) = recording.as_ref() else {
            return;
        };
        if asking.resume {
            recording.set_paused(false);
        }
        recording.view()
    };
    log::info!("{:?} cancelled; the take is kept", asking.action);
    show(view, cx, |_, cx| cx.notify());
    state.refresh_tray_menu();
}

/// Discard at once, but pause rather than delete for the undo window, so
/// Undo can bring the take back.
/// The controls take the keyboard meanwhile, so Enter can discard at once;
/// it goes back afterwards.
fn discard_with_undo(state: &Rc<State>, cx: &mut AsyncApp) {
    let undo_window = state.new_undo_window();
    let window = state.settings.borrow().undo_window();
    let until = Instant::now() + window;
    let (view, os_window) = {
        let mut slot = state.recording.borrow_mut();
        let Some(recording) = slot.as_mut() else {
            return;
        };
        if recording.recorder.is_none() {
            return;
        }
        let resume = recording.set_paused(true);
        recording.color_border(BORDER_DISCARDED);
        let os_window = recording
            .controls
            .as_ref()
            .and_then(|c| c.popup.os_window());
        let give_back = platform_window::foreground_window().filter(|w| Some(*w) != os_window);
        recording.discarded = Some(Discarded {
            resume,
            undo_window,
            give_back,
        });
        (recording.view(), os_window)
    };
    log::info!(
        "recording discarded; it can be undone for {} s",
        window.as_secs()
    );
    show(view, cx, |bar, cx| {
        bar.set_mode(BarMode::Discarded { until }, cx)
    });
    if let Some(os_window) = os_window {
        platform_window::bring_to_front(os_window);
    }
    state.recording_changed(cx);
    let state = state.clone();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(window).await;
        let due = state
            .recording
            .borrow()
            .as_ref()
            .and_then(|r| r.discarded)
            .is_some_and(|d| d.undo_window == undo_window);
        if due {
            discard_recording(&state, cx);
        }
    })
    .detach();
}

/// Undo a discard, or keep the take a restart replaced.
pub(in crate::app) fn undo(state: &Rc<State>, cx: &mut AsyncApp) {
    let discarded = {
        let mut slot = state.recording.borrow_mut();
        slot.as_mut()
            .and_then(|r| Some((r.discarded.take()?, r.view())))
    };
    if let Some((discarded, view)) = discarded {
        if let Some(recording) = state.recording.borrow().as_ref() {
            if discarded.resume {
                recording.set_paused(false);
            }
            recording.color_border_for_clock();
        }
        if let Some(window) = discarded.give_back {
            platform_window::bring_to_front(window);
        }
        log::info!("discard undone");
        show(view, cx, |bar, cx| {
            bar.set_mode(BarMode::Controls, cx);
            bar.notice("Discard undone", Duration::from_secs(2), cx);
        });
        state.recording_changed(cx);
        return;
    }
    let Some(previous) = state.previous_take.take() else {
        return;
    };
    let view = state.recording.borrow().as_ref().and_then(Recording::view);
    let name = previous
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    state.recording_changed(cx);
    let state = state.clone();
    cx.spawn(async move |cx| {
        let path = previous.path.clone();
        keep_previous_take(previous, cx).await;
        match view {
            Some(view) => {
                // A notification now would be in the recording.
                show(Some(view), cx, |bar, cx| {
                    bar.offer_previous(None, cx);
                    bar.notice(format!("Kept {name}"), Duration::from_secs(3), cx);
                })
            }
            None => state.notify("Previous take kept", name, Some(path)),
        }
    })
    .detach();
}

/// Give a take a restart replaced its final name.
pub(in crate::app) async fn keep_previous_take(previous: PreviousTake, cx: &mut AsyncApp) {
    let path = previous.path;
    let kept = cx
        .background_executor()
        .spawn({
            let path = path.clone();
            async move { std::fs::rename(files::partial_path(&path), &path) }
        })
        .await;
    match kept {
        Ok(()) => log::info!("kept the previous take as {}", path.display()),
        Err(e) => log::error!("could not keep {}: {e}", path.display()),
    }
}

/// Start again with the same area and settings, as a new file (PRD §7.7).
/// With `keep_previous`, the take it replaces is finished and kept for
/// the undo window, so Undo can keep it; otherwise it is deleted.
async fn restart_recording(state: &Rc<State>, keep_previous: bool, cx: &mut AsyncApp) {
    let taken = state.recording.borrow_mut().as_mut().and_then(|recording| {
        let recorder = recording.recorder.take()?;
        Some((recorder, recording.options.clone()))
    });
    let Some((recorder, options)) = taken else {
        return;
    };
    let dir = state.settings.borrow().recording_dir();
    let started_at = Local::now().naive_local();
    let restarted = cx
        .background_executor()
        .spawn(async move {
            let kept = finish_replaced_take(recorder, &options.path, keep_previous);
            start_take(&dir, started_at, options).map(|take| (take, kept))
        })
        .await;
    let mut slot = state.recording.borrow_mut();
    match (restarted, slot.as_mut()) {
        (Ok((mut take, kept)), Some(recording)) => {
            let ended = take.recorder.ended();
            let watched = take.path.clone();
            let previous = std::mem::replace(&mut recording.path, take.path);
            let switched = (recording.options.system_sound, recording.options.microphone);
            recording.options = take.options;
            recording.recorder = Some(take.recorder);
            // The sound may have been switched while restarting.
            recording.set_sound(Source::System, switched.0);
            recording.set_sound(Source::Microphone, switched.1);
            recording.clock.set(Clock::new(Instant::now()));
            let view = recording.view();
            drop(slot);
            log::info!("recording restarted");
            offer_replaced_take(state, previous, kept, view, cx);
            state.recording_changed(cx);
            watch_recording(state, ended, watched, cx);
        }
        // Shuttercrab quit while restarting: the new recording is not wanted.
        (Ok((take, _)), None) => {
            drop(slot);
            cx.background_executor()
                .spawn(async move {
                    if let Ok(summary) = take.recorder.stop() {
                        let _ = std::fs::remove_file(summary.path);
                    }
                })
                .detach();
        }
        (Err(e), _) => {
            let recording = slot.take();
            drop(slot);
            if let Some(mut recording) = recording {
                recording.close_controls(cx);
            }
            stop_following(state);
            state.recording_changed(cx);
            let failure = Failure::new(
                "Could not restart recording.",
                format!("restart: {}", e.detail),
            )
            .of_recording();
            log::error!("{}", failure.detail);
            state.notify(failure.title(), failure.message, None);
        }
    }
}

/// Stop the take a restart replaces, written to `partial`. Its file is
/// kept when `keep_previous` and it finished cleanly, and deleted
/// otherwise. Returns whether it was kept.
fn finish_replaced_take(recorder: RecorderProcess, partial: &Path, keep_previous: bool) -> bool {
    let finished = recorder.stop();
    if let Err(e) = &finished {
        log::warn!("the replaced take did not stop cleanly: {e:#}");
    }
    let kept = keep_previous && finished.is_ok();
    if !kept {
        let _ = std::fs::remove_file(partial);
    }
    kept
}

/// After a restart, say so in the controls. A `kept` take, whose final name
/// is `previous`, is offered to Undo until its undo window closes.
fn offer_replaced_take(
    state: &Rc<State>,
    previous: PathBuf,
    kept: bool,
    view: Option<Entity<RecordBar>>,
    cx: &mut AsyncApp,
) {
    let until = Instant::now() + state.settings.borrow().undo_window();
    if kept {
        let undo_window = state.new_undo_window();
        state.previous_take.replace(Some(PreviousTake {
            path: previous,
            undo_window,
        }));
        forget_previous_take_later(
            state,
            undo_window,
            until.saturating_duration_since(Instant::now()),
            cx,
        );
    }
    show(view, cx, |bar, cx| {
        bar.offer_previous(kept.then_some(until), cx);
        bar.notice("Restarted", Duration::from_secs(2), cx);
    });
}

/// Delete the take a restart replaced once its undo window closes, unless
/// Undo kept it or a newer window replaced it.
fn forget_previous_take_later(
    state: &Rc<State>,
    undo_window: Token,
    window: Duration,
    cx: &mut AsyncApp,
) {
    let state = state.clone();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(window).await;
        let due = state
            .previous_take
            .borrow()
            .as_ref()
            .is_some_and(|p| p.undo_window == undo_window);
        if !due {
            return;
        }
        let Some(previous) = state.previous_take.take() else {
            return;
        };
        let view = state.recording.borrow().as_ref().and_then(Recording::view);
        show(view, cx, |bar, cx| bar.offer_previous(None, cx));
        state.recording_changed(cx);
        let partial = files::partial_path(&previous.path);
        match std::fs::remove_file(&partial) {
            Ok(()) => log::info!("the replaced take is deleted"),
            Err(e) => log::error!("could not delete {}: {e}", partial.display()),
        }
    })
    .detach();
}
