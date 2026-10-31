//! Live streaming (issue #66): choosing what to stream as for a recording,
//! the stream's bar with its quality, opening the room in the streaming
//! helper, the link on the clipboard, and the bar saying when a viewer
//! watches, until Stop or the viewer leaves.

use super::{
    State,
    failure::Failure,
    recording::{
        BORDER_RECORDING, BORDER_WAITING, Border, Chosen, Following, choose_area, ready, show,
        show_border, sound, stop_following,
    },
    windows::open_main,
};
use crate::{
    capture_choice::CaptureTarget,
    helpers::streamer::{StreamEvent, StreamProcess, StreamStart},
    recording::Clock,
    settings::Settings,
    ui::main_window::Page,
    ui::record_bar::{BarMode, RecordBarEvent, Sound},
};
use futures::{StreamExt as _, channel::mpsc::UnboundedReceiver};
use gpui_kit::{AsyncApp, ClipboardItem};
use shuttercrab_capture::{MonitorId, record::RecordOptions};
use std::{
    cell::{Cell, RefCell},
    ops::ControlFlow,
    path::PathBuf,
    rc::Rc,
    time::Instant,
};

/// A stream's frame rate: as smooth as the screen, for text that scrolls.
const STREAM_FPS: u32 = 60;

/// A live stream in progress.
pub(super) struct Streaming {
    /// `None` once it is stopping.
    process: Option<StreamProcess>,
    /// What to share: whoever has it can watch.
    link: String,
    controls: Option<super::recording::Controls>,
    border: Border,
    /// From when the viewer came.
    clock: Rc<Cell<Clock>>,
    codec: Option<String>,
}

impl Streaming {
    fn view(&self) -> Option<gpui_kit::Entity<crate::ui::record_bar::RecordBar>> {
        self.controls.as_ref().map(|c| c.view.clone())
    }
}

/// The host key to stream with; or, with none kept yet, Settings opened on
/// Streaming, where it is pasted in, and `None`.
pub(super) fn host_key_or_ask(state: &Rc<State>, cx: &mut AsyncApp) -> Option<String> {
    let key = state.settings.borrow().host_key();
    if key.is_none() {
        log::info!("no host key to stream with: Settings opened on Streaming");
        open_main(state, Page::StreamingSettings, cx);
    }
    key
}

/// Stream `target`, chosen as for a recording, from `monitor`: the stream's
/// bar, then the room, then the link on the clipboard. It runs until Stop or
/// the viewer leaves.
pub(super) async fn stream(
    state: &Rc<State>,
    target: CaptureTarget,
    monitor: MonitorId,
    pressed: Instant,
    cx: &mut AsyncApp,
) -> Result<(), Failure> {
    if state.recording.borrow().is_some() || state.streaming.borrow().is_some() {
        log::info!("already recording or streaming; the stream is not started");
        return Ok(());
    }
    let Some(host_key) = host_key_or_ask(state, cx) else {
        return Ok(());
    };
    let Some(Chosen {
        region,
        info,
        window,
    }) = choose_area(state, target, monitor, pressed, cx).await?
    else {
        log::info!("stream cancelled");
        return Ok(());
    };
    let border: Border = Rc::new(RefCell::new(show_border(
        &info,
        region,
        window,
        BORDER_WAITING,
    )));
    let following = window.map(|w| Following::start(state, w, border.clone()));
    let settings = state.settings.borrow().clone();
    let clock = Rc::new(Cell::new(Clock::new(Instant::now())));
    let ControlFlow::Continue(opened) = ready(
        state,
        &info,
        (region, window),
        clock.clone(),
        Some(settings.stream_quality),
        cx,
    )
    .await
    else {
        log::info!("stream cancelled in the ready bar");
        return Ok(());
    };
    let (controls, requests) = opened.unzip();
    let (sound, quality) = match &controls {
        Some(controls) => controls
            .view
            .read_with(cx, |bar, _| (bar.sound().clone(), bar.quality())),
        None => (sound(&settings), None),
    };
    let quality = quality.unwrap_or(settings.stream_quality);
    let chosen = Chosen {
        region,
        info,
        window,
    };
    let (options, source) = options(&chosen, &settings, sound);
    let start = StreamStart::new(options, source, quality, host_key);
    let opened = cx
        .background_executor()
        .spawn(async move { StreamProcess::start(start) })
        .await;
    let (process, link, events) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            if let Some(controls) = controls {
                controls.popup.close(cx);
            }
            // Said in the main window, where Stream's steps are, rather than
            // in a notification that is easily missed.
            log::error!("streamer: {e:#}");
            let why = format!("{e:#}");
            open_main(state, Page::Home, cx);
            let main = state.main_window.borrow().clone();
            if let Some((window, view)) = main {
                let _ = window.update(cx, |_, _, cx| {
                    view.update(cx, |view, cx| view.stream_failed(&why, cx));
                });
            }
            return Ok(());
        }
    };
    log::info!(
        "streaming {} at {}; waiting for a viewer",
        match chosen.region {
            Some(r) => format!("{}×{} at {},{}", r.width, r.height, r.x, r.y),
            None => "the display".to_string(),
        },
        quality.name()
    );
    let streaming = Streaming {
        process: Some(process),
        link,
        controls,
        border,
        clock,
        codec: None,
    };
    begin(state, streaming, (requests, events), cx);
    if let Some(following) = following {
        following.keep();
    }
    Ok(())
}

/// What to record for `chosen`, as the settings and the bar's `sound` say,
/// and its size in physical pixels.
fn options(chosen: &Chosen, settings: &Settings, sound: Sound) -> (RecordOptions, (u32, u32)) {
    let Chosen {
        region,
        info,
        window,
    } = chosen;
    let app_process = window.and_then(shuttercrab_platform::window::sound_process);
    let options = RecordOptions {
        monitor: info.id,
        // A window is streamed wherever it goes.
        region: if window.is_some() { None } else { *region },
        window: *window,
        fps: STREAM_FPS,
        include_cursor: settings.record_cursor,
        system_sound: sound.system,
        sound_process: app_process.filter(|_| sound.app_only),
        microphone: sound.microphone,
        microphone_device: sound.device,
        // The bar can switch either on mid-stream.
        sound_track: true,
        path: PathBuf::new(),
    };
    let source = region.map_or((info.bounds.width, info.bounds.height), |r| {
        (r.width, r.height)
    });
    (options, source)
}

/// The room is open: put the link on the clipboard, show the bar waiting
/// for a viewer, and handle the bar's `requests` and the stream's `events`.
fn begin(
    state: &Rc<State>,
    streaming: Streaming,
    (requests, events): (
        Option<UnboundedReceiver<RecordBarEvent>>,
        UnboundedReceiver<StreamEvent>,
    ),
    cx: &mut AsyncApp,
) {
    copy(&streaming.link, cx);
    show(streaming.view(), cx, |bar, cx| {
        bar.set_mode(BarMode::Controls, cx);
        bar.stream_status(false, "Link copied · waiting for a viewer", cx);
    });
    state.streaming.replace(Some(streaming));
    state.refresh_tray_menu();
    if let Some(requests) = requests {
        handle_bar(state.clone(), requests, cx);
    }
    handle_events(state.clone(), events, cx);
}

/// Put `link` on the clipboard.
fn copy(link: &str, cx: &mut AsyncApp) {
    let item = ClipboardItem::new_string(link.to_string());
    cx.update(|cx| cx.write_to_clipboard(item));
}

/// Do what the stream's bar asks until the stream ends.
fn handle_bar(
    state: Rc<State>,
    mut requests: UnboundedReceiver<RecordBarEvent>,
    cx: &mut AsyncApp,
) {
    cx.spawn(async move |cx| {
        while let Some(asked) = requests.next().await {
            let process = |f: &dyn Fn(&StreamProcess)| {
                if let Some(process) = state
                    .streaming
                    .borrow()
                    .as_ref()
                    .and_then(|s| s.process.as_ref())
                {
                    f(process);
                }
            };
            match asked {
                RecordBarEvent::Quality(quality) => {
                    log::info!("stream quality: {}", quality.name());
                    process(&|p| p.set_quality(quality));
                }
                RecordBarEvent::SetSound(source, on) => process(&|p| p.set_sound(source, on)),
                RecordBarEvent::CopyLink => {
                    let shared = state
                        .streaming
                        .borrow()
                        .as_ref()
                        .map(|s| (s.link.clone(), s.view()));
                    if let Some((link, view)) = shared {
                        copy(&link, cx);
                        show(view, cx, |bar, cx| {
                            bar.notice("Link copied", std::time::Duration::from_secs(2), cx);
                        });
                    }
                }
                RecordBarEvent::Stop => stop_streaming(&state, "Stopped.".into(), cx),
                _ => {}
            }
            if state.streaming.borrow().is_none() {
                break;
            }
        }
    })
    .detach();
}

/// Show what the stream does: the viewer coming, how it goes, and its end.
fn handle_events(state: Rc<State>, mut events: UnboundedReceiver<StreamEvent>, cx: &mut AsyncApp) {
    cx.spawn(async move |cx| {
        while let Some(event) = events.next().await {
            match event {
                StreamEvent::Viewer { codec } => {
                    log::info!("a viewer is watching, as {codec}");
                    let view = {
                        let mut streaming = state.streaming.borrow_mut();
                        let Some(streaming) = streaming.as_mut() else {
                            break;
                        };
                        streaming.clock.set(Clock::new(Instant::now()));
                        if let Some(frame) = streaming.border.borrow_mut().as_mut()
                            && let Err(e) = frame.recolor(BORDER_RECORDING)
                        {
                            log::warn!("could not redraw the stream's border: {e:#}");
                        }
                        streaming.codec = Some(short(&codec));
                        streaming.view()
                    };
                    show(view, cx, |bar, cx| {
                        bar.stream_status(true, short(&codec), cx)
                    });
                }
                StreamEvent::Stats(stats) => {
                    let (view, codec) = match state.streaming.borrow().as_ref() {
                        Some(s) => (s.view(), s.codec.clone().unwrap_or_default()),
                        None => break,
                    };
                    let detail = match stats.round_trip_ms {
                        Some(ms) => format!("{codec} · {} · {ms} ms", stats.quality.name()),
                        None => format!("{codec} · {}", stats.quality.name()),
                    };
                    show(view, cx, |bar, cx| bar.stream_status(true, detail, cx));
                }
                StreamEvent::Ended(why) => {
                    stop_streaming(&state, why, cx);
                    break;
                }
            }
        }
    })
    .detach();
}

/// A codec's short name, for the bar: `AV1` of `AV1, 10 bits`.
fn short(codec: &str) -> String {
    codec.split(',').next().unwrap_or(codec).to_string()
}

/// End the stream, if one runs: close its bar and border, and let the
/// helper finish in the background. `why` goes in the notification.
pub(super) fn stop_streaming(state: &State, why: String, cx: &mut AsyncApp) {
    let Some(mut streaming) = state.streaming.take() else {
        return;
    };
    log::info!("stream ended: {why}");
    streaming.border.borrow_mut().take();
    if let Some(controls) = streaming.controls.take() {
        controls.popup.close(cx);
    }
    stop_following(state);
    if let Some(process) = streaming.process.take() {
        cx.background_executor()
            .spawn(async move { process.stop() })
            .detach();
    }
    state.refresh_tray_menu();
    state.show_hidden_main();
    if why != "Stopped." {
        state.notify("The stream ended", why, None);
    }
}

/// The streamed window was closed: the stream ends.
pub(super) fn window_closed(state: &State, cx: &mut AsyncApp) -> ControlFlow<()> {
    if state.streaming.borrow().is_none() {
        return ControlFlow::Continue(());
    }
    stop_streaming(state, "The streamed window was closed.".into(), cx);
    ControlFlow::Break(())
}
