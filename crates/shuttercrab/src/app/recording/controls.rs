//! The recording controls: showing them next to what is recorded,
//! changing what they show, and doing what they ask.

use super::{
    request, stop_recording,
    takes::{confirm, keep_take},
    toggle_pause, undo,
};
use crate::{
    app::{State, screenshot::exclude_from_capture},
    recording::{self, Clock},
    ui::popup::{self, Activation},
    ui::record_bar::{
        Destructive, RECORD_BAR_HEIGHT, RECORD_BAR_WIDTH, RecordBar, RecordBarEvent, RecordKeys,
        STREAM_BAR_WIDTH, Sound,
    },
};
use futures::{StreamExt as _, channel::mpsc::UnboundedReceiver};
use gpui_kit::{AppContext as _, AsyncApp, Entity};
use shuttercrab_capture::{MonitorInfo, PhysicalRect, record::Microphone};
use shuttercrab_platform::window as platform_window;
use shuttercrab_stream::Quality;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// The recording controls' window and view.
pub(in crate::app) struct Controls {
    pub(in crate::app) popup: popup::Popup,
    pub(in crate::app) view: Entity<RecordBar>,
    /// Where the bar is, physical virtual-desktop pixels.
    pub(super) rect: PhysicalRect,
}

/// Change what the controls show, outside any borrow of the state.
pub(in crate::app) fn show(
    view: Option<Entity<RecordBar>>,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut RecordBar, &mut gpui_kit::Context<RecordBar>),
) {
    if let Some(view) = view {
        view.update(cx, f);
    }
}

/// The recording controls' distance from the recorded area, logical pixels.
pub(super) const CONTROLS_GAP: f32 = 12.0;

/// Show the recording controls next to `region` (physical pixels relative
/// to the monitor; `None` for all of it), excluded from capture: a live
/// stream's, at `stream` quality, if given. If Windows cannot exclude them
/// and they would cover the region, they are not shown (PRD §16); the
/// hotkey and the tray still stop the recording.
pub(super) fn open_controls(
    info: &MonitorInfo,
    region: Option<PhysicalRect>,
    clock: Rc<Cell<Clock>>,
    (keys, sound, microphones): (RecordKeys, Sound, Vec<Microphone>),
    stream: Option<Quality>,
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
    let work = platform_window::work_area(info.id).unwrap_or(b);
    let width = if stream.is_some() {
        STREAM_BAR_WIDTH
    } else {
        RECORD_BAR_WIDTH
    };
    let size = (
        (width * scale).round() as u32,
        (RECORD_BAR_HEIGHT * scale).round() as u32,
    );
    let gap = (CONTROLS_GAP * scale).round() as u32;
    let (rect, covers) = recording::controls_rect(work, recorded, size, gap);

    let view = Rc::new(RefCell::new(None));
    let slot = view.clone();
    let opened = popup::open(info, rect, Activation::OnClick, cx, move |window, cx| {
        let bar = cx.new(|cx| {
            let bar = RecordBar::new(clock, keys, window, cx).ready(sound, microphones);
            match stream {
                Some(quality) => bar.streaming(quality),
                None => bar,
            }
        });
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
    let excluded = match popup.os_window() {
        Some(os_window) => {
            platform_window::round_corners(os_window);
            exclude_from_capture(os_window, "recording controls")
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
    Some((Controls { popup, view, rect }, requests))
}

/// Do what the recording controls ask until they close.
pub(super) fn handle_controls(
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
                RecordBarEvent::KeepRecording => keep_take(&state, cx),
                RecordBarEvent::Undo => undo(&state, cx),
                RecordBarEvent::SetAppOnly(on) => {
                    if let Some(recording) = state.recording.borrow_mut().as_mut() {
                        recording.set_app_only(on);
                    }
                }
                RecordBarEvent::SetSound(source, on) => {
                    if let Some(recording) = state.recording.borrow_mut().as_mut() {
                        recording.set_sound(source, on);
                    }
                }
                // Only the ready bar asks these, before recording; only a
                // stream's bar the last two.
                RecordBarEvent::Start
                | RecordBarEvent::Close
                | RecordBarEvent::ChooseMicrophone
                | RecordBarEvent::Quality(_)
                | RecordBarEvent::CopyLink => {}
            }
            if state.recording.borrow().is_none() {
                break;
            }
        }
    })
    .detach();
}
