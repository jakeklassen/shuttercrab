//! Recording: choosing the area, the ready bar and its microphone list,
//! the countdown, the border and the controls; pausing, restarting,
//! discarding and undoing; switching the sound; and finishing the file.
//!
//! This module holds the recording in progress, starting a take, the
//! countdown, and pausing. The rest is split by responsibility:
//!
//! - `ready`: choosing what to record, and the ready bar with its
//!   microphone list.
//! - `border`: the dashed border around what is recorded.
//! - `controls`: showing the recording controls, and doing what they ask.
//! - `follow`: following a recorded window with the border and the bar.
//! - `window_changes`: the recorded window moving, going out of sight and
//!   coming back, and closing.
//! - `takes`: asking before a take is thrown away, Restart, Discard and
//!   Undo.
//! - `finish`: stopping or discarding the recording, finishing its file,
//!   and telling the user.

mod border;
mod controls;
mod finish;
mod follow;
mod ready;
mod takes;
mod window_changes;

pub(super) use border::{BORDER_RECORDING, BORDER_WAITING, Border, show_border};
pub(super) use controls::{Controls, show};
pub(super) use finish::{finish_recording, report_recording, stop_recording};
pub(super) use follow::{Followed, Following, stop_following};
pub(super) use ready::{Chosen, choose_area, ready, sound};
pub(super) use takes::{PreviousTake, keep_previous_take, request, undo};
pub(super) use window_changes::window_changed;

use super::{State, failure::Failure, screenshot::exclude_from_capture};
use crate::{
    capture_choice::CaptureTarget,
    files,
    generation::Generation,
    helpers::recorder::RecorderProcess,
    recording::{self, Clock},
    selection::to_physical,
    ui::countdown::{COUNTDOWN_HEIGHT, COUNTDOWN_WIDTH, Countdown, CountdownEvent},
    ui::popup::{self, Activation},
    ui::record_bar::{BarMode, RecordBar, RecordBarEvent},
};
use border::BORDER_PAUSED;
use chrono::{Local, NaiveDateTime};
use controls::handle_controls;
use follow::{Waiting, waiting_on};
use futures::{StreamExt as _, channel::mpsc::UnboundedReceiver};
use gpui_kit::{AppContext as _, AsyncApp, Entity};
use shuttercrab_capture::{
    MonitorId, MonitorInfo, PhysicalRect,
    record::{RecordOptions, Source},
};
use shuttercrab_platform::window as platform_window;
use std::{
    cell::{Cell, RefCell},
    ops::ControlFlow,
    path::{Path, PathBuf},
    rc::Rc,
    time::Instant,
};
use takes::{Asking, Discarded, keep_take};

/// A recording in progress.
pub(super) struct Recording {
    /// `None` only while Restart swaps in a new one.
    pub(super) recorder: Option<RecorderProcess>,
    /// Where the finished file goes; until then it is written to
    /// [`files::partial_path`] of it.
    pub(super) path: PathBuf,
    /// What is recorded, for Restart.
    options: RecordOptions,
    /// The recorded window's app, whose sound can be recorded alone.
    app_process: Option<u32>,
    /// The output length so far, shared with the controls.
    pub(super) clock: Rc<Cell<Clock>>,
    /// The recording controls, if they are on screen.
    controls: Option<Controls>,
    /// A question the controls are asking, if any.
    asking: Option<Asking>,
    /// A discard that can still be undone, if any.
    pub(super) discarded: Option<Discarded>,
    /// The dashed border around what is recorded, if it is shown.
    border: Border,
    /// Minimising or hiding the recorded window paused the recording;
    /// showing it again resumes it.
    paused_out_of_sight: bool,
    /// Each time the window goes out of sight, so a resume waiting for a
    /// restored window to settle knows if it went again.
    hidings: Generation,
}

impl Recording {
    /// Redraw the controls after the clock changed.
    fn refresh_controls(&self, cx: &mut AsyncApp) {
        if let Some(controls) = &self.controls {
            controls.view.update(cx, |_, cx| cx.notify());
        }
    }

    /// Close the controls and remove the border.
    fn close_controls(&mut self, cx: &mut AsyncApp) {
        self.border.borrow_mut().take();
        if let Some(controls) = self.controls.take() {
            controls.popup.close(cx);
        }
    }

    /// The controls' view, to change what it shows.
    fn view(&self) -> Option<Entity<RecordBar>> {
        self.controls.as_ref().map(|c| c.view.clone())
    }

    /// Pause or resume the recorder and the clock. Returns whether that
    /// changed anything.
    fn set_paused(&self, paused: bool) -> bool {
        let Some(recorder) = &self.recorder else {
            return false;
        };
        let (mut clock, now) = (self.clock.get(), Instant::now());
        if clock.is_paused() == paused {
            return false;
        }
        if paused {
            recorder.pause();
            clock.pause(now);
        } else {
            recorder.resume();
            clock.resume(now);
        }
        self.clock.set(clock);
        self.color_border_for_clock();
        true
    }

    /// Record only the window's app's share of the system sound, or all
    /// of it, for this take and any restart of it.
    fn set_app_only(&mut self, on: bool) {
        let process = self.app_process.filter(|_| on);
        if self.options.sound_process == process {
            return;
        }
        self.options.sound_process = process;
        if let Some(recorder) = &self.recorder {
            recorder.set_sound_process(process);
        }
        log::info!(
            "system sound: {}",
            if on { "the app's only" } else { "all of it" }
        );
    }

    /// Switch `source` on or off, for this take and any restart of it.
    fn set_sound(&mut self, source: Source, on: bool) {
        let options = &mut self.options;
        let was = match source {
            Source::System => std::mem::replace(&mut options.system_sound, on),
            Source::Microphone => std::mem::replace(&mut options.microphone, on),
        };
        if was == on {
            return;
        }
        if let Some(recorder) = &self.recorder {
            recorder.set_sound(source, on);
        }
        log::info!(
            "{} switched {}",
            match source {
                Source::System => "system sound",
                Source::Microphone => "the microphone",
            },
            if on { "on" } else { "off" }
        );
    }

    /// Draw the border in `color`.
    fn color_border(&self, color: [u8; 3]) {
        if let Some(frame) = self.border.borrow_mut().as_mut()
            && let Err(e) = frame.recolor(color)
        {
            log::warn!("could not redraw the recording border: {e:#}");
        }
    }

    /// Draw the border red while recording, amber while paused.
    fn color_border_for_clock(&self) {
        let paused = self.clock.get().is_paused();
        self.color_border(if paused {
            BORDER_PAUSED
        } else {
            BORDER_RECORDING
        });
    }
}

/// Record `target`: all of `monitor`, or an area chosen on frozen frames of
/// every monitor, `monitor` first. Show the controls, then start the
/// recorder. It runs until [`stop_recording`] or [`discard_recording`](finish::discard_recording).
pub(super) async fn record(
    state: &Rc<State>,
    target: CaptureTarget,
    monitor: MonitorId,
    pressed: Instant,
    cx: &mut AsyncApp,
) -> Result<(), Failure> {
    if state.recording.borrow().is_some() || state.streaming.borrow().is_some() {
        log::info!("already recording or streaming; the new request is ignored");
        return Ok(());
    }
    let Some(chosen) = choose_area(state, target, monitor, pressed, cx).await? else {
        log::info!("recording cancelled");
        return Ok(());
    };
    let (region, info, window) = (chosen.region, chosen.info, chosen.window);
    // The border shows what will be recorded: grey until recording starts.
    // Around a window, it follows the window from now on.
    let border: Border = Rc::new(RefCell::new(show_border(
        &info,
        region,
        window,
        BORDER_WAITING,
    )));
    let following = window.map(|w| Following::start(state, w, border.clone()));
    let settings = state.settings.borrow().clone();
    let clock = Rc::new(Cell::new(Clock::new(Instant::now())));
    let ControlFlow::Continue(opened) =
        ready(state, &info, (region, window), clock.clone(), None, cx).await
    else {
        log::info!("recording cancelled in the ready bar");
        return Ok(());
    };
    let (controls, requests) = opened.unzip();
    let countdown = settings.countdown();
    if countdown > 0 && !count_down(state, &info, region, countdown, cx).await {
        if let Some(controls) = controls {
            controls.popup.close(cx);
        }
        log::info!("recording cancelled during the countdown");
        return Ok(());
    }
    let chosen = Instant::now();
    // As switched in the bar, which can change during the countdown.
    let sound = match &controls {
        Some(controls) => controls.view.read_with(cx, |bar, _| bar.sound().clone()),
        None => sound(&settings),
    };
    // The window's app, whose sound can be recorded alone.
    let app_process = window.and_then(platform_window::sound_process);

    let dir = settings.recording_dir();
    let options = RecordOptions {
        // The area may be on another monitor than the one first under the
        // pointer.
        monitor: info.id,
        // A window is recorded wherever it goes.
        region: if window.is_some() { None } else { region },
        window,
        fps: settings.record_fps(),
        include_cursor: settings.record_cursor,
        system_sound: sound.system,
        // Only the recorded window's app's, if so chosen.
        sound_process: app_process.filter(|_| sound.app_only),
        microphone: sound.microphone,
        microphone_device: sound.device,
        // The controls can switch either on mid-recording.
        sound_track: true,
        // `start_take` names the file.
        path: PathBuf::new(),
    };
    let started_at = Local::now().naive_local();
    let started = cx
        .background_executor()
        .spawn(async move { start_take(&dir, started_at, options) })
        .await
        .map_err(Failure::of_recording);
    let take = match started {
        Ok(take) => take,
        Err(failure) => {
            if let Some(controls) = controls {
                controls.popup.close(cx);
            }
            return Err(failure);
        }
    };
    log::info!(
        "recording {} at {} fps, {} ms after the choice",
        match region {
            Some(r) => format!("{}×{} at {},{}", r.width, r.height, r.x, r.y),
            None => "the display".to_string(),
        },
        take.options.fps,
        chosen.elapsed().as_millis()
    );
    clock.set(Clock::new(Instant::now()));
    let view = controls.as_ref().map(|c| c.view.clone());
    show(view, cx, |bar, cx| bar.set_mode(BarMode::Controls, cx));
    let recording = Recording {
        recorder: Some(take.recorder),
        path: take.path,
        options: take.options,
        clock,
        controls,
        asking: None,
        discarded: None,
        border,
        app_process,
        paused_out_of_sight: false,
        hidings: Generation::default(),
    };
    begin(state, recording, requests, cx);
    if let Some(following) = following {
        following.keep();
    }
    Ok(())
}

/// A take that has started.
struct Take {
    recorder: RecorderProcess,
    /// The file's final name; until the take is finished it is written to
    /// [`files::partial_path`] of it.
    path: PathBuf,
    options: RecordOptions,
}

/// Start a take of what `options` say, into a new file in `dir` named for
/// `started_at`; that file replaces `options.path`. Blocks while the
/// helper process starts, so it runs off the main thread.
fn start_take(
    dir: &Path,
    started_at: NaiveDateTime,
    options: RecordOptions,
) -> Result<Take, Failure> {
    // Said before anything starts: what to install (on Linux, a GStreamer
    // plugin package), in the notification.
    if let Some(message) = shuttercrab_capture::record::missing_software() {
        return Err(Failure::new(
            message.clone(),
            format!("recorder: {message}"),
        ));
    }
    let path = files::new_recording(dir, started_at).map_err(|e| {
        Failure::new(
            format!(
                "Could not save to {}. Check the folder in Settings.",
                dir.display()
            ),
            format!("{e:#}"),
        )
    })?;
    let options = RecordOptions {
        path: files::partial_path(&path),
        ..options
    };
    let recorder = RecorderProcess::start(options.clone())
        .map_err(|e| Failure::new("Could not start recording.", format!("recorder: {e:#}")))?;
    Ok(Take {
        recorder,
        path,
        options,
    })
}

/// Make `recording` the one in progress: show its clock and colour, tell
/// the tray, handle the controls' `requests`, and watch for the take ending
/// by itself.
fn begin(
    state: &Rc<State>,
    mut recording: Recording,
    requests: Option<UnboundedReceiver<RecordBarEvent>>,
    cx: &mut AsyncApp,
) {
    let ended = recording.recorder.as_mut().and_then(RecorderProcess::ended);
    let path = recording.path.clone();
    recording.refresh_controls(cx);
    recording.color_border_for_clock();
    state.recording.replace(Some(recording));
    state.recording_changed(cx);
    if let Some(requests) = requests {
        handle_controls(state.clone(), requests, cx);
    }
    finish_if_it_ends(state, ended, path, cx);
}

/// Notice when the take written to `path` ends by itself (the display went
/// away, the device was lost, the disk filled up: PRD §25) and finish it as
/// if stopped: what was recorded is saved, and the notification says why.
/// `ended` is the recorder's [`RecorderProcess::ended`]; it also resolves when the
/// take is stopped, discarded or restarted, and then the take is no longer
/// the one recording.
fn finish_if_it_ends(
    state: &Rc<State>,
    ended: Option<impl std::future::Future<Output = ()> + 'static>,
    path: PathBuf,
    cx: &mut AsyncApp,
) {
    let Some(ended) = ended else {
        return;
    };
    let state = state.clone();
    cx.spawn(async move |cx| {
        ended.await;
        let by_itself = state
            .recording
            .borrow()
            .as_ref()
            .is_some_and(|r| r.path == path && r.recorder.is_some());
        if by_itself {
            log::warn!("the recording ended by itself");
            stop_recording(&state, cx);
        }
    })
    .detach();
}

/// Windows says the displays changed: if the recorded one is gone, end the
/// recording with what was recorded; otherwise the recorder reads its white
/// level again (HDR switched on or off).
pub(super) fn displays_changed(state: &State) {
    let recording = state.recording.borrow();
    let Some(recording) = recording.as_ref() else {
        return;
    };
    let Some(recorder) = &recording.recorder else {
        return;
    };
    // A window is recorded on whichever display it is.
    if recording.options.window.is_some()
        || shuttercrab_capture::record::attached(recording.options.monitor)
    {
        log::info!("displays changed; the recorded one is still attached");
        recorder.display_changed();
    } else {
        log::warn!("the recorded display is gone");
        recorder.display_gone();
    }
}

/// Count down `seconds` over the middle of `region` (physical pixels
/// relative to the monitor; `None` for all of it) before recording starts.
/// The countdown takes the keyboard, so Enter and Escape work, and gives it
/// back when it ends. Returns whether to start.
async fn count_down(
    state: &State,
    info: &MonitorInfo,
    region: Option<PhysicalRect>,
    seconds: u32,
    cx: &mut AsyncApp,
) -> bool {
    let (b, scale) = (info.bounds, info.scale_factor);
    let area = shuttercrab_capture::record::recordable(region, b.width, b.height);
    let (w, h) = (
        to_physical(COUNTDOWN_WIDTH, scale) as i32,
        to_physical(COUNTDOWN_HEIGHT, scale) as i32,
    );
    let centre = |start: i32, side: u32, size: i32, limit: u32| {
        (start + (side as i32 - size) / 2).clamp(0, (limit as i32 - size).max(0))
    };
    let rect = PhysicalRect::new(
        b.x + centre(area.x, area.width, w, b.width),
        b.y + centre(area.y, area.height, h, b.height),
        w as u32,
        h as u32,
    );
    let give_back = platform_window::foreground_window();
    let slot = Rc::new(RefCell::new(None));
    let view = slot.clone();
    let opened = popup::open(info, rect, Activation::Take, cx, move |window, cx| {
        let countdown = cx.new(|cx| Countdown::new(seconds, window, cx));
        view.replace(Some(countdown.clone()));
        countdown
    });
    let (popup, mut events) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            log::warn!("could not show the countdown; recording at once: {e}");
            return true;
        }
    };
    if let Some(os_window) = popup.os_window() {
        platform_window::round_corners(os_window);
        exclude_from_capture(os_window, "countdown");
    }
    waiting_on(state, slot.take().map(Waiting::Countdown));
    let event = events.next().await.unwrap_or(CountdownEvent::Cancel);
    waiting_on(state, None);
    popup.close(cx);
    if let Some(window) = give_back {
        platform_window::bring_to_front(window);
    }
    event == CountdownEvent::Go
}

/// Pause the recording, or resume it if it is paused. While the controls
/// ask before throwing the take away, this answers "keep it"; while a
/// discard can be undone, it does nothing.
pub(super) fn toggle_pause(state: &State, cx: &mut AsyncApp) {
    let (asking, discarded) = match &*state.recording.borrow() {
        Some(r) => (r.asking.is_some(), r.discarded.is_some()),
        None => return,
    };
    if asking {
        return keep_take(state, cx);
    }
    if discarded {
        return;
    }
    let view = {
        let mut recording = state.recording.borrow_mut();
        let Some(recording) = recording.as_mut() else {
            return;
        };
        let paused = !recording.clock.get().is_paused();
        recording.set_paused(paused);
        // Resumed by hand while minimised: restoring has nothing to do.
        recording.paused_out_of_sight = false;
        let clock = recording.clock.get();
        if paused {
            log::info!(
                "recording paused at {}",
                recording::clock(clock.elapsed(Instant::now()))
            );
        } else {
            log::info!("recording resumed");
        }
        recording.view()
    };
    show(view, cx, |bar, cx| bar.pause_reason(None, cx));
    state.refresh_tray_menu();
}
