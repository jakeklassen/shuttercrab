//! Before recording: choosing what to record, and the ready bar with its
//! microphone list.

use super::{
    Controls,
    controls::{CONTROLS_GAP, open_controls},
    follow::{FollowingBar, Waiting, waiting_on},
};
use crate::{
    app::{
        State,
        failure::Failure,
        screenshot::{Choosing, exclude_from_capture, monitor_info, select},
    },
    capture_choice::CaptureTarget,
    recording::Clock,
    settings::Settings,
    ui::choice_menu::{CHOICE_MENU_WIDTH, ChoiceMenu, ChoiceMenuEvent, choice_menu_height},
    ui::overlay::{Mode, OverlayEvent},
    ui::popup::{self, Activation},
    ui::record_bar::{RecordBarEvent, RecordKeys, Sound},
};
use futures::{StreamExt as _, channel::mpsc::UnboundedReceiver};
use gpui_kit::{AppContext as _, AsyncApp};
use shuttercrab_capture::{MonitorId, MonitorInfo, PhysicalRect};
use shuttercrab_platform::window as platform_window;
use shuttercrab_stream::Quality;
use shuttercrab_types::WindowId;
use std::{cell::Cell, ops::ControlFlow, rc::Rc, time::Instant};

/// What a recording records, as chosen.
pub(in crate::app) struct Chosen {
    /// Physical pixels relative to the monitor; `None` for all of it. For a
    /// window, where it is now.
    pub(in crate::app) region: Option<PhysicalRect>,
    pub(in crate::app) info: MonitorInfo,
    /// The window to record itself, wherever it goes (its `HWND`), when
    /// one was chosen as a window.
    pub(in crate::app) window: Option<WindowId>,
}

/// What to record for `target`: an area or all of a monitor, or a window.
/// `None` when the user cancels the selection.
pub(in crate::app) async fn choose_area(
    state: &Rc<State>,
    target: CaptureTarget,
    monitor: MonitorId,
    pressed: Instant,
    cx: &mut AsyncApp,
) -> Result<Option<Chosen>, Failure> {
    if target == CaptureTarget::Display {
        return Ok(Some(Chosen {
            region: None,
            info: monitor_info(state, monitor).await?,
            window: None,
        }));
    }
    // A freeform shape is recorded as an area; so is a window clicked
    // while choosing an area.
    let mode = match target {
        CaptureTarget::Window => Mode::Window,
        _ => Mode::Area,
    };
    let first = state.capture.freeze_monitor(monitor, false).await?;
    let (frozen, event) = select(state, first, Choosing::Recording, mode, pressed, cx).await?;
    let (region, window) = match event {
        OverlayEvent::Selected(rect) => (Some(rect), None),
        OverlayEvent::Display => (None, None),
        OverlayEvent::Window { os_window, visible } => (
            Some(visible),
            (target == CaptureTarget::Window).then_some(os_window),
        ),
        // Area and Window modes draw no shapes.
        OverlayEvent::Cancelled | OverlayEvent::ModeChanged(_) | OverlayEvent::Shape(_) => {
            return Ok(None);
        }
    };
    Ok(Some(Chosen {
        region,
        info: frozen.info,
        window,
    }))
}

/// The sound the settings record; the ready bar starts from it.
pub(in crate::app) fn sound(settings: &Settings) -> Sound {
    Sound {
        system: settings.record_system_sound,
        app_only: settings.record_app_sound_only,
        microphone: settings.record_microphone,
        device: settings.microphone.clone(),
    }
}

/// Show the ready bar next to `region` (or `window`) and wait for Start; it
/// stays as the controls, so it is excluded before the first frame. With
/// `stream`, it is a live stream's bar, starting at that quality. Breaks if
/// cancelled; without the bar (it cannot be shown) recording starts at
/// once.
pub(in crate::app) async fn ready(
    state: &State,
    info: &MonitorInfo,
    (region, window): (Option<PhysicalRect>, Option<WindowId>),
    clock: Rc<Cell<Clock>>,
    stream: Option<Quality>,
    cx: &mut AsyncApp,
) -> ControlFlow<(), Option<(Controls, UnboundedReceiver<RecordBarEvent>)>> {
    let give_back = platform_window::foreground_window();
    let microphones = shuttercrab_capture::record::microphones()
        .inspect_err(|e| log::warn!("could not list the microphones: {e:#}"))
        .unwrap_or_default();
    let (keys, sound) = {
        let settings = state.settings.borrow();
        (record_keys(&settings), sound(&settings))
    };
    let opened = open_controls(info, region, clock, (keys, sound, microphones), stream, cx);
    let Some((controls, mut requests)) = opened else {
        return ControlFlow::Continue(None);
    };
    // A recorded window's app's sound can be had alone.
    if window.is_some() {
        controls.view.update(cx, |bar, cx| bar.offer_app_sound(cx));
    }
    // A recorded window takes the bar along when it moves.
    if let Some(followed) = state.followed.borrow().as_ref() {
        let gap = (CONTROLS_GAP * info.scale_factor).round() as u32;
        let size = (controls.rect.width, controls.rect.height);
        followed
            .bar
            .set(controls.popup.os_window().map(|os_window| FollowingBar {
                os_window,
                size,
                gap,
            }));
    }
    waiting_on(state, Some(Waiting::Ready(controls.view.clone())));
    let ready = get_ready(state, info, &controls, &mut requests, give_back, cx).await;
    waiting_on(state, None);
    if ready {
        ControlFlow::Continue(Some((controls, requests)))
    } else {
        controls.popup.close(cx);
        ControlFlow::Break(())
    }
}

/// Wait for the ready bar's Start or Cancel. The bar has the keyboard
/// meanwhile, and gives it back to `give_back` then. Returns whether to
/// start. A microphone chosen there is kept in the settings, for the next
/// recording too.
async fn get_ready(
    state: &State,
    info: &MonitorInfo,
    controls: &Controls,
    requests: &mut UnboundedReceiver<RecordBarEvent>,
    give_back: Option<WindowId>,
    cx: &mut AsyncApp,
) -> bool {
    if let Some(os_window) = controls.popup.os_window() {
        platform_window::bring_to_front(os_window);
    }
    let start = loop {
        match requests.next().await {
            Some(RecordBarEvent::Start) => break true,
            Some(RecordBarEvent::Close) | None => break false,
            Some(RecordBarEvent::ChooseMicrophone) => {
                if let Some(device) = choose_microphone(info, controls, cx).await {
                    state.update_settings(|settings| settings.microphone = device, cx);
                }
            }
            // The bar keeps the sound as switched.
            Some(_) => {}
        }
    };
    if let Some(window) = give_back {
        platform_window::bring_to_front(window);
    }
    start
}

/// The microphone list's distance from the bar, logical pixels.
const LIST_GAP: f32 = 4.0;

/// Open the microphone list under its button in the bar (over it when
/// there is no room below), and use what is chosen. The list has the
/// keyboard; the bar gets it back. Returns the microphone chosen (`None`
/// inside for Windows' default), if one was.
async fn choose_microphone(
    info: &MonitorInfo,
    controls: &Controls,
    cx: &mut AsyncApp,
) -> Option<Option<String>> {
    let (choices, chosen, button) = controls.view.read_with(cx, |bar, _| {
        let (choices, chosen) = bar.microphone_choices();
        (choices, chosen, bar.list_button())
    });
    let scale = info.scale_factor;
    let to_physical = |logical: f32| (logical * scale).round() as i32;
    let (w, h) = (
        to_physical(CHOICE_MENU_WIDTH),
        to_physical(choice_menu_height(choices.len())),
    );
    // Where the bar is now: it may have followed a recorded window.
    let bar = controls
        .popup
        .os_window()
        .and_then(|os_window| platform_window::client_bounds(os_window).ok())
        .unwrap_or(controls.rect);
    let gap = to_physical(LIST_GAP);
    let work = platform_window::work_area(info.id).unwrap_or(bar);
    let x = (bar.x + to_physical(f32::from(button.origin.x)))
        .min(work.right() - w)
        .max(work.x);
    let below = bar.bottom() + gap;
    let y = if below + h <= work.bottom() {
        below
    } else {
        bar.y - gap - h
    };
    let rect = PhysicalRect::new(x, y, w as u32, h as u32);
    let opened = popup::open(info, rect, Activation::Take, cx, move |window, cx| {
        cx.new(|cx| ChoiceMenu::new(choices, chosen, window, cx))
    });
    let (popup, mut events) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            log::warn!("could not show the microphone list: {e}");
            return None;
        }
    };
    if let Some(os_window) = popup.os_window() {
        platform_window::round_corners(os_window);
        exclude_from_capture(os_window, "microphone list");
    }
    let event = events.next().await.unwrap_or(ChoiceMenuEvent::Cancel);
    popup.close(cx);
    if let Some(os_window) = controls.popup.os_window() {
        platform_window::bring_to_front(os_window);
    }
    let ChoiceMenuEvent::Chose(index) = event else {
        return None;
    };
    Some(controls.view.update(cx, |bar, cx| {
        bar.choose_microphone(index, cx);
        bar.sound().device.clone()
    }))
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
