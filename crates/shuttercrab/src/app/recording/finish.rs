//! Ending a recording: stopping it and finishing its file, or discarding
//! it, and telling the user.

use super::{stop_following, takes::stop_asking};
use crate::{
    app::{State, failure::Failure, windows::show_recording_in_main},
    files, heap,
    helpers::recorder::RecorderProcess,
    recording,
};
use gpui_kit::AsyncApp;
use shuttercrab_capture::record::RecordingSummary;
use shuttercrab_platform::window as platform_window;
use std::{path::PathBuf, rc::Rc};

/// Take the recording in progress and its recorder, closing the controls.
/// `None` if there is none, or while a restart is swapping recorders.
fn end_recording(state: &State, cx: &mut AsyncApp) -> Option<(RecorderProcess, PathBuf)> {
    if state.recording.borrow().as_ref()?.recorder.is_none() {
        log::info!("the recording is restarting; the request is ignored");
        return None;
    }
    // Hand the keyboard back if the controls were asking or counting down.
    stop_asking(state, cx);
    let mut recording = state.recording.borrow_mut().take()?;
    recording.close_controls(cx);
    stop_following(state);
    if let Some(window) = recording.discarded.and_then(|d| d.give_back) {
        platform_window::bring_to_front(window);
    }
    state.recording_changed(cx);
    Some((recording.recorder?, recording.path))
}

/// Stop the recording in progress, finish its file, and say where it is.
/// Stopping always keeps the take, even one the controls were asking about
/// or one discarded moments ago.
pub(in crate::app) fn stop_recording(state: &Rc<State>, cx: &mut AsyncApp) {
    let Some((recorder, path)) = end_recording(state, cx) else {
        return;
    };
    let state = state.clone();
    cx.spawn(async move |cx| {
        let result = finish_recording(recorder, path, cx).await;
        // Ready to play, as Snipping Tool opens each recording.
        let opens = result
            .as_ref()
            .ok()
            .filter(|_| state.settings.borrow().show_recordings_in_window)
            .map(|summary| (summary.path.clone(), (summary.width, summary.height)));
        report_recording(&state, result);
        if let Some((path, size)) = opens {
            show_recording_in_main(&state, path, size, cx);
        }
        heap::log_memory_soon("after a recording", cx);
    })
    .detach();
}

/// Stop the recording in progress and delete it: no file is left (PRD
/// §7.7).
pub(super) fn discard_recording(state: &Rc<State>, cx: &mut AsyncApp) {
    let Some((recorder, path)) = end_recording(state, cx) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            let partial = files::partial_path(&path);
            if let Err(e) = recorder.stop() {
                log::warn!("the discarded recording did not stop cleanly: {e:#}");
            }
            match std::fs::remove_file(&partial) {
                Ok(()) => log::info!("recording discarded"),
                Err(e) => log::error!("could not delete {}: {e}", partial.display()),
            }
        })
        .detach();
}

/// Stop `recorder` and move the file to `path`, its final name, off the
/// main thread. On failure, the unfinished file is deleted: it cannot be
/// played.
pub(in crate::app) async fn finish_recording(
    recorder: RecorderProcess,
    path: PathBuf,
    cx: &mut AsyncApp,
) -> Result<RecordingSummary, Failure> {
    cx.background_executor()
        .spawn(async move {
            let partial = files::partial_path(&path);
            let summary = recorder.stop().and_then(|summary| {
                std::fs::rename(&partial, &path)?;
                Ok(summary)
            });
            match summary {
                Ok(summary) => Ok(RecordingSummary { path, ..summary }),
                Err(e) => {
                    let _ = std::fs::remove_file(&partial);
                    Err(Failure::new(
                        "The recording could not be finished.",
                        format!("finishing {}: {e:#}", path.display()),
                    )
                    .of_recording())
                }
            }
        })
        .await
}

/// Log a finished recording and tell the user where it went.
pub(in crate::app) fn report_recording(state: &State, result: Result<RecordingSummary, Failure>) {
    match result {
        Ok(summary) => {
            let name = summary
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            log::info!(
                "saved {name}: {}×{}, {:.2} s, {} frames ({} dropped, {} skipped), {:.1} s paused, {} encoder",
                summary.width,
                summary.height,
                summary.duration.as_secs_f64(),
                summary.frames,
                summary.dropped_busy,
                summary.skipped_rate,
                summary.paused.as_secs_f64(),
                if summary.hardware_encoder {
                    "hardware"
                } else {
                    "software"
                }
            );
            let length = recording::clock(summary.duration);
            match &summary.interrupted {
                // Unexpected, so always said, whatever the settings.
                Some(why) => {
                    log::warn!("the recording ended by itself: {why:?}");
                    state.notify(
                        format!("Recording stopped: {}", why.describe()),
                        format!("What was recorded is saved: {length}, {name}."),
                        Some(summary.path.clone()),
                    );
                }
                None if state.settings.borrow().notify_after_recording => {
                    state.notify(
                        format!("Recording saved · {length}"),
                        format!("{name}, {} × {}", summary.width, summary.height),
                        Some(summary.path.clone()),
                    );
                }
                None => {}
            }
        }
        // Failures are always reported.
        Err(failure) => {
            log::error!("{}", failure.detail);
            state.notify(failure.title(), failure.message, None);
        }
    }
}
