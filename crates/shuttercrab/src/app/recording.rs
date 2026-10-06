//! Recording: choosing the area, the countdown, the border and the
//! controls; pausing, restarting, discarding and undoing; and finishing the
//! file.

use super::{
    State,
    failure::Failure,
    screenshot::{exclude_from_capture, monitor_info, select},
};
use crate::{
    capture_choice::CaptureTarget,
    countdown::{COUNTDOWN_HEIGHT, COUNTDOWN_WIDTH, Countdown, CountdownEvent},
    files, heap,
    overlay::{Mode, OverlayEvent},
    popup::{self, Activation},
    record_bar::{
        BarMode, Destructive, RECORD_BAR_HEIGHT, RECORD_BAR_WIDTH, RecordBar, RecordBarEvent,
        RecordKeys,
    },
    recorder_process::RecorderProcess,
    recording::{self, Clock},
    settings::Settings,
};
use chrono::{Local, NaiveDateTime};
use futures::{StreamExt as _, channel::mpsc::UnboundedReceiver};
use gpui_kit::{AppContext as _, AsyncApp, Entity};
use shuttercrab_capture::{
    MonitorId, MonitorInfo, PhysicalRect,
    record::{RecordOptions, RecordingSummary},
};
use shuttercrab_platform::{
    frame::{Frame, FrameStyle, Rect as FrameRect},
    window as platform_window,
};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

/// A finished take that a restart replaced: deleted when its undo window
/// closes, unless Undo keeps it.
pub(super) struct PreviousTake {
    /// Its final name; until kept it stays at [`files::partial_path`].
    path: PathBuf,
    generation: u64,
}

/// The controls are asking before a take is thrown away.
#[derive(Clone, Copy, Debug)]
struct Asking {
    action: Destructive,
    /// The recording was running when asked, and resumes if kept.
    resume: bool,
    /// The window that had the keyboard before the bar took it to ask.
    give_back: Option<isize>,
}

/// A discard that can still be undone: the recording is paused meanwhile.
#[derive(Clone, Copy, Debug)]
pub(super) struct Discarded {
    /// The recording was running, and resumes if the discard is undone.
    resume: bool,
    generation: u64,
    /// The window that had the keyboard before the controls took it.
    give_back: Option<isize>,
}

/// A recording in progress.
pub(super) struct Recording {
    /// `None` only while Restart swaps in a new one.
    pub(super) recorder: Option<RecorderProcess>,
    /// Where the finished file goes; until then it is written to
    /// [`files::partial_path`] of it.
    pub(super) path: PathBuf,
    /// What is recorded, for Restart.
    options: RecordOptions,
    /// The output length so far, shared with the controls.
    pub(super) clock: Rc<Cell<Clock>>,
    /// The recording controls, if they are on screen.
    controls: Option<Controls>,
    /// A question the controls are asking, if any.
    asking: Option<Asking>,
    /// A discard that can still be undone, if any.
    pub(super) discarded: Option<Discarded>,
    /// The dashed border around the recorded area, if it is shown.
    frame: Option<Frame>,
}

/// The border's colours (RGB): recording, paused, and discarded.
const FRAME_RECORDING: [u8; 3] = [0xE5, 0x48, 0x4D];
const FRAME_PAUSED: [u8; 3] = [0xF5, 0xA5, 0x24];
const FRAME_DISCARDED: [u8; 3] = [0x9D, 0x9D, 0x9D];
/// Before recording starts, during a countdown.
const FRAME_WAITING: [u8; 3] = FRAME_DISCARDED;

/// The border's look, logical pixels.
const FRAME_THICKNESS: f32 = 2.0;
const FRAME_DASH: f32 = 8.0;
const FRAME_GAP: f32 = 5.0;

/// The recording controls' window and view.
struct Controls {
    popup: popup::Popup,
    view: Entity<RecordBar>,
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
        self.frame = None;
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
        self.color_frame_for_clock();
        true
    }

    /// Draw the border in `color`.
    fn color_frame(&self, color: [u8; 3]) {
        if let Some(frame) = &self.frame
            && let Err(e) = frame.recolor(color)
        {
            log::warn!("could not redraw the recording border: {e:#}");
        }
    }

    /// Draw the border red while recording, amber while paused.
    fn color_frame_for_clock(&self) {
        let paused = self.clock.get().is_paused();
        self.color_frame(if paused {
            FRAME_PAUSED
        } else {
            FRAME_RECORDING
        });
    }
}

/// Change what the controls show, outside any borrow of the state.
fn show(
    view: Option<Entity<RecordBar>>,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut RecordBar, &mut gpui_kit::Context<RecordBar>),
) {
    if let Some(view) = view {
        view.update(cx, f);
    }
}

/// The recording controls' distance from the recorded area, logical pixels.
const CONTROLS_GAP: f32 = 12.0;

/// Record `target`: all of `monitor`, or an area chosen on frozen frames of
/// every monitor, `monitor` first. Show the controls, then start the
/// recorder. It runs until [`stop_recording`] or [`discard_recording`].
pub(super) async fn record(
    state: &Rc<State>,
    target: CaptureTarget,
    monitor: MonitorId,
    pressed: Instant,
    cx: &mut AsyncApp,
) -> Result<(), Failure> {
    if state.recording.borrow().is_some() {
        log::info!("already recording; the new request is ignored");
        return Ok(());
    }
    let Some((region, info)) = choose_area(state, target, monitor, pressed, cx).await? else {
        log::info!("recording cancelled");
        return Ok(());
    };
    // The border shows what will be recorded: grey until recording starts.
    let countdown = state.settings.borrow().countdown();
    let waiting = if countdown > 0 {
        FRAME_WAITING
    } else {
        FRAME_RECORDING
    };
    let frame = show_frame(&info, region, waiting);
    if countdown > 0 && !count_down(&info, region, countdown, cx).await {
        log::info!("recording cancelled during the countdown");
        return Ok(());
    }
    let chosen = Instant::now();
    let clock = Rc::new(Cell::new(Clock::new(chosen)));
    // The controls come first, so they are excluded before the first frame.
    let keys = record_keys(&state.settings.borrow());
    let (controls, requests) = match open_controls(&info, region, clock.clone(), keys, cx) {
        Some((controls, requests)) => (Some(controls), Some(requests)),
        None => (None, None),
    };

    let settings = state.settings.borrow().clone();
    let dir = settings.recording_dir();
    let options = RecordOptions {
        // The area may be on another monitor than the one first under the
        // pointer.
        monitor: info.id,
        region,
        fps: settings.record_fps(),
        include_cursor: settings.record_cursor,
        system_sound: settings.record_system_sound,
        microphone: settings.record_microphone,
        microphone_device: settings.microphone.clone(),
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
    let recording = Recording {
        recorder: Some(take.recorder),
        path: take.path,
        options: take.options,
        clock,
        controls,
        asking: None,
        discarded: None,
        frame,
    };
    begin(state, recording, requests, cx);
    Ok(())
}

/// What to record for `target`: the region (`None` for all of the monitor)
/// and its monitor. `None` when the user cancels the selection.
async fn choose_area(
    state: &Rc<State>,
    target: CaptureTarget,
    monitor: MonitorId,
    pressed: Instant,
    cx: &mut AsyncApp,
) -> Result<Option<(Option<PhysicalRect>, MonitorInfo)>, Failure> {
    match target {
        CaptureTarget::Display => Ok(Some((None, monitor_info(state, monitor).await?))),
        // A window or a freeform shape is recorded as an area.
        CaptureTarget::Area | CaptureTarget::Window | CaptureTarget::Freeform => {
            let first = state.capture.freeze_monitor(monitor, false).await?;
            let (frame, event) = select(state, first, false, Mode::Area, true, pressed, cx).await?;
            let region = match event {
                OverlayEvent::Selected(rect) => Some(rect),
                OverlayEvent::Display => None,
                OverlayEvent::Window { visible, .. } => Some(visible),
                // Area mode draws no shapes.
                OverlayEvent::Cancelled | OverlayEvent::ModeChanged(_) | OverlayEvent::Shape(_) => {
                    return Ok(None);
                }
            };
            Ok(Some((region, frame.info)))
        }
    }
}

/// The controls' key hints, from the hotkey settings.
fn record_keys(settings: &Settings) -> RecordKeys {
    RecordKeys {
        pause: settings.pause_hotkey.clone(),
        stop: settings.record_hotkey.clone(),
        restart: settings.restart_hotkey.clone(),
        discard: settings.discard_hotkey.clone(),
        undo: settings.undo_hotkey.clone(),
    }
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
    let watched = recording.path.clone();
    recording.refresh_controls(cx);
    recording.color_frame_for_clock();
    state.recording.replace(Some(recording));
    state.recording_changed(cx);
    if let Some(requests) = requests {
        handle_controls(state.clone(), requests, cx);
    }
    watch_recording(state, ended, watched, cx);
}

/// Show the recording controls next to `region` (physical pixels relative
/// to the monitor; `None` for all of it), excluded from capture. If
/// Windows cannot exclude them and they would cover the region, they are
/// not shown (PRD §16); the hotkey and the tray still stop the recording.
fn open_controls(
    info: &MonitorInfo,
    region: Option<PhysicalRect>,
    clock: Rc<Cell<Clock>>,
    keys: RecordKeys,
    cx: &mut AsyncApp,
) -> Option<(Controls, UnboundedReceiver<RecordBarEvent>)> {
    let (b, scale) = (info.bounds, info.scale_factor);
    let recorded = shuttercrab_capture::record::recordable(region, b.width, b.height);
    let recorded = PhysicalRect::new(
        b.x + recorded.x,
        b.y + recorded.y,
        recorded.width,
        recorded.height,
    );
    let work = platform_window::work_area(info.id.0)
        .map(|(x, y, w, h)| PhysicalRect::new(x, y, w, h))
        .unwrap_or(b);
    let size = (
        (RECORD_BAR_WIDTH * scale).round() as u32,
        (RECORD_BAR_HEIGHT * scale).round() as u32,
    );
    let gap = (CONTROLS_GAP * scale).round() as u32;
    let (rect, covers) = recording::controls_rect(work, recorded, size, gap);

    let view = Rc::new(RefCell::new(None));
    let slot = view.clone();
    let opened = popup::open(info, rect, Activation::OnClick, cx, move |window, cx| {
        let bar = cx.new(|cx| RecordBar::new(clock, keys, window, cx));
        slot.replace(Some(bar.clone()));
        bar
    });
    let (popup, requests) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            log::error!("could not show the recording controls: {e}");
            return None;
        }
    };
    let view = view.take()?;
    let excluded = match popup.hwnd() {
        Some(hwnd) => {
            platform_window::round_corners(hwnd);
            exclude_from_capture(hwnd, "recording controls")
        }
        None => false,
    };
    if !excluded && covers {
        log::warn!("the recording controls would be recorded; recording without them");
        popup.close(cx);
        return None;
    }
    log::info!(
        "recording controls at {},{} ({}, {})",
        rect.x,
        rect.y,
        if covers {
            "over the area"
        } else {
            "outside the area"
        },
        if excluded {
            "excluded from capture"
        } else {
            "not excluded"
        }
    );
    Some((Controls { popup, view }, requests))
}

/// Notice when the take written to `path` ends by itself (the display went
/// away, the device was lost, the disk filled up: PRD §25) and finish it as
/// if stopped: what was recorded is saved, and the notification says why.
/// `ended` is the recorder's [`RecorderProcess::ended`]; it also resolves when the
/// take is stopped, discarded or restarted, and then the take is no longer
/// the one recording.
fn watch_recording(
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
    if shuttercrab_capture::record::attached(recording.options.monitor) {
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
    info: &MonitorInfo,
    region: Option<PhysicalRect>,
    seconds: u32,
    cx: &mut AsyncApp,
) -> bool {
    let (b, scale) = (info.bounds, info.scale_factor);
    let area = shuttercrab_capture::record::recordable(region, b.width, b.height);
    let (w, h) = (
        (COUNTDOWN_WIDTH * scale).round() as i32,
        (COUNTDOWN_HEIGHT * scale).round() as i32,
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
    let opened = popup::open(info, rect, Activation::Take, cx, move |window, cx| {
        cx.new(|cx| Countdown::new(seconds, window, cx))
    });
    let (popup, mut events) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            log::warn!("could not show the countdown; recording at once: {e}");
            return true;
        }
    };
    if let Some(hwnd) = popup.hwnd() {
        platform_window::round_corners(hwnd);
        exclude_from_capture(hwnd, "countdown");
    }
    let event = events.next().await.unwrap_or(CountdownEvent::Cancel);
    popup.close(cx);
    if let Some(window) = give_back {
        platform_window::bring_to_front(window);
    }
    event == CountdownEvent::Go
}

/// Show the dashed border around `region` (physical pixels relative to the
/// monitor; `None` for all of it) in `color`, before recording starts, so
/// it is excluded from the first frame.
fn show_frame(info: &MonitorInfo, region: Option<PhysicalRect>, color: [u8; 3]) -> Option<Frame> {
    let (b, scale) = (info.bounds, info.scale_factor);
    let area = shuttercrab_capture::record::recordable(region, b.width, b.height);
    let px = |logical: f32| ((logical * scale).round() as u32).max(1);
    let style = FrameStyle {
        thickness: px(FRAME_THICKNESS),
        dash: px(FRAME_DASH),
        gap: px(FRAME_GAP),
    };
    let area = FrameRect::new(b.x + area.x, b.y + area.y, area.width, area.height);
    let bounds = FrameRect::new(b.x, b.y, b.width, b.height);
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

/// Do what the recording controls ask until they close.
fn handle_controls(
    state: Rc<State>,
    mut requests: UnboundedReceiver<RecordBarEvent>,
    cx: &mut AsyncApp,
) {
    cx.spawn(async move |cx| {
        while let Some(asked) = requests.next().await {
            match asked {
                RecordBarEvent::TogglePause => toggle_pause(&state, cx),
                RecordBarEvent::Stop => stop_recording(&state, cx),
                RecordBarEvent::Restart => request(&state, Destructive::Restart, cx).await,
                RecordBarEvent::Discard => request(&state, Destructive::Discard, cx).await,
                RecordBarEvent::Confirm => confirm(&state, cx).await,
                RecordBarEvent::Cancel => keep_take(&state, cx),
                RecordBarEvent::Undo => undo(&state, cx),
            }
            if state.recording.borrow().is_none() {
                break;
            }
        }
    })
    .detach();
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
        let recording = state.recording.borrow();
        let Some(recording) = recording.as_ref() else {
            return;
        };
        let paused = !recording.clock.get().is_paused();
        recording.set_paused(paused);
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
    show(view, cx, |_, cx| cx.notify());
    state.refresh_tray_menu();
}

/// Discard or Restart was asked for: ask first, or act at once and offer
/// undo, as the settings say. Asking for the same action again while the
/// controls ask confirms it.
pub(super) async fn request(state: &Rc<State>, action: Destructive, cx: &mut AsyncApp) {
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
    let (view, hwnd, length) = {
        let mut slot = state.recording.borrow_mut();
        let Some(recording) = slot.as_mut() else {
            return;
        };
        if recording.recorder.is_none() {
            return;
        }
        let resume = recording.set_paused(true);
        let hwnd = recording.controls.as_ref().and_then(|c| c.popup.hwnd());
        let give_back = platform_window::foreground_window().filter(|w| Some(*w) != hwnd);
        recording.asking = Some(Asking {
            action,
            resume,
            give_back,
        });
        let length = recording.clock.get().elapsed(Instant::now());
        (recording.view(), hwnd, length)
    };
    log::info!(
        "asking before {action:?} of a {} take",
        recording::clock(length)
    );
    show(view, cx, |bar, cx| {
        bar.set_mode(BarMode::Confirm(action, length), cx)
    });
    if let Some(hwnd) = hwnd {
        platform_window::bring_to_front(hwnd);
    }
    state.refresh_tray_menu();
}

/// Stop asking: the controls show the actions again and the keyboard goes
/// back to where it was. Returns what was being asked.
fn stop_asking(state: &State, cx: &mut AsyncApp) -> Option<Asking> {
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
async fn confirm(state: &Rc<State>, cx: &mut AsyncApp) {
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
fn keep_take(state: &State, cx: &mut AsyncApp) {
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
    let generation = state.next_generation();
    let window = state.settings.borrow().undo_window();
    let until = Instant::now() + window;
    let (view, hwnd) = {
        let mut slot = state.recording.borrow_mut();
        let Some(recording) = slot.as_mut() else {
            return;
        };
        if recording.recorder.is_none() {
            return;
        }
        let resume = recording.set_paused(true);
        recording.color_frame(FRAME_DISCARDED);
        let hwnd = recording.controls.as_ref().and_then(|c| c.popup.hwnd());
        let give_back = platform_window::foreground_window().filter(|w| Some(*w) != hwnd);
        recording.discarded = Some(Discarded {
            resume,
            generation,
            give_back,
        });
        (recording.view(), hwnd)
    };
    log::info!(
        "recording discarded; it can be undone for {} s",
        window.as_secs()
    );
    show(view, cx, |bar, cx| {
        bar.set_mode(BarMode::Discarded { until }, cx)
    });
    if let Some(hwnd) = hwnd {
        platform_window::bring_to_front(hwnd);
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
            .is_some_and(|d| d.generation == generation);
        if due {
            discard_recording(&state, cx);
        }
    })
    .detach();
}

/// Undo a discard, or keep the take a restart replaced.
pub(super) fn undo(state: &Rc<State>, cx: &mut AsyncApp) {
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
            recording.color_frame_for_clock();
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
pub(super) async fn keep_previous_take(previous: PreviousTake, cx: &mut AsyncApp) {
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
    if let Some(window) = recording.discarded.and_then(|d| d.give_back) {
        platform_window::bring_to_front(window);
    }
    state.recording_changed(cx);
    Some((recording.recorder?, recording.path))
}

/// Stop the recording in progress, finish its file, and say where it is.
/// Stopping always keeps the take, even one the controls were asking about
/// or one discarded moments ago.
pub(super) fn stop_recording(state: &Rc<State>, cx: &mut AsyncApp) {
    let Some((recorder, path)) = end_recording(state, cx) else {
        return;
    };
    let state = state.clone();
    cx.spawn(async move |cx| {
        let result = finish_recording(recorder, path, cx).await;
        report_recording(&state, result);
        heap::log_memory_soon("after a recording", cx);
    })
    .detach();
}

/// Stop the recording in progress and delete it: no file is left (PRD
/// §7.7).
fn discard_recording(state: &Rc<State>, cx: &mut AsyncApp) {
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
            recording.options = take.options;
            recording.recorder = Some(take.recorder);
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
        let generation = state.next_generation();
        state.previous_take.replace(Some(PreviousTake {
            path: previous,
            generation,
        }));
        forget_previous_take_later(
            state,
            generation,
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
    generation: u64,
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
            .is_some_and(|p| p.generation == generation);
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

/// Stop `recorder` and move the file to `path`, its final name, off the
/// main thread. On failure, the unfinished file is deleted: it cannot be
/// played.
pub(super) async fn finish_recording(
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
pub(super) fn report_recording(state: &State, result: Result<RecordingSummary, Failure>) {
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
