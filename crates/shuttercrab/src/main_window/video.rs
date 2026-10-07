//! A recording shown in the main window, as Snipping Tool shows one: its
//! picture fitted to the window, and a bar at the bottom to play or pause
//! it, with the time, a slider to move through it, its length, and the
//! volume, whose button opens a slider of its own above it. It opens
//! paused on its first picture: sound that starts by surprise is as
//! unwelcome as sound recorded by surprise.
//!
//! Keys: Space plays or pauses, Left and Right go back and forward five
//! seconds, Home and End go to the start and the end, Up and Down change
//! the volume, M mutes. Escape closes the volume's slider.

use super::{FOOTER_HEIGHT, MainWindow, TOOLBAR_HEIGHT};
use crate::{
    palette::{border, coral, hover, muted},
    pixels,
    playback::{Command, PlayerLink, Update},
    shot_view::{MARGIN, Xy},
};
use futures::StreamExt as _;
use gpui_kit::{
    AppContext as _, ClickEvent, Context, Div, Entity, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, ParentElement as _, RenderImage, Role, Stateful,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _, Window,
    assets::IconName,
    component::{
        Icon,
        slider::{Slider, SliderEvent, SliderState},
    },
    deferred, div, img,
    prelude::FluentBuilder as _,
    px, rgb,
};
use std::{path::PathBuf, sync::Arc, time::Duration};

/// How far Left and Right move, and Up and Down change the volume.
const SKIP: Duration = Duration::from_secs(5);
const VOLUME_STEP: u32 = 5;

/// The seek slider's range: it moves through thousandths of the recording,
/// its length unknown when the slider is made.
const SEEK_RANGE: f32 = 1000.;

/// The play bar's height and its widest, and its gap from the window's
/// bottom edge, logical pixels.
const BAR_HEIGHT: f32 = 64.;
const BAR_WIDTH: f32 = 760.;
const BAR_GAP: f32 = 16.;

/// A recording the window shows.
pub(super) struct Video {
    /// Tells this video's updates from those of one shown before.
    id: u64,
    pub(super) path: PathBuf,
    link: PlayerLink,
    /// Its pictures' size, physical pixels: from the recorder, then from
    /// the file once open.
    pub(super) size: Option<(u32, u32)>,
    duration: Duration,
    position: Duration,
    playing: bool,
    /// The picture shown, and those it replaced, which the next frame lets
    /// go of.
    frame: Option<Arc<RenderImage>>,
    stale: Vec<Arc<RenderImage>>,
    /// The picture size last asked of the player, physical pixels.
    asked: Option<(u32, u32)>,
    /// Why it cannot be played, if it cannot.
    failed: Option<String>,
    seek: Entity<SliderState>,
    volume: Entity<SliderState>,
    /// The volume's slider is open.
    volume_open: bool,
    /// The seek slider is being dragged: the player's position waits.
    scrubbing: bool,
    _updates: Task<()>,
    _sliders: [Subscription; 2],
}

impl Video {
    /// Let go of its pictures. The player closes as the link drops.
    pub(super) fn release(self, window: &mut Window) {
        for image in self.frame.into_iter().chain(self.stale) {
            let _ = window.drop_image(image);
        }
    }

    /// Let go of the pictures replaced since the last frame.
    pub(super) fn drop_stale(&mut self, window: &mut Window) {
        for image in self.stale.drain(..) {
            let _ = window.drop_image(image);
        }
    }

    /// The time `value` on the seek slider stands for.
    fn time_at(&self, value: f32) -> Duration {
        self.duration
            .mul_f64(f64::from(value.clamp(0., SEEK_RANGE) / SEEK_RANGE))
    }

    /// Where on the seek slider `at` is.
    fn value_at(&self, at: Duration) -> f32 {
        if self.duration.is_zero() {
            return 0.;
        }
        (at.as_secs_f64() / self.duration.as_secs_f64()).clamp(0., 1.) as f32 * SEEK_RANGE
    }

    pub(super) fn is_playing(&self) -> bool {
        self.playing
    }
}

/// `at` as Snipping Tool shows times: hours, minutes and seconds.
pub fn clock(at: Duration) -> String {
    let s = at.as_secs();
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// The size a `size` picture shows at in `area`, logical pixels: as big as
/// fits inside the margins, but no bigger than its own pixels.
pub fn fitted(size: (u32, u32), area: Xy, scale: f32) -> Xy {
    let natural = Xy::new(size.0 as f32 / scale, size.1 as f32 / scale);
    let room = Xy::new(
        (area.x - 2. * MARGIN).max(1.),
        (area.y - 2. * MARGIN).max(1.),
    );
    let k = (room.x / natural.x).min(room.y / natural.y).min(1.);
    Xy::new(natural.x * k, natural.y * k)
}

/// The space a recording shows in: the window less the toolbar and the
/// footer.
fn video_area(window: &Window) -> Xy {
    let viewport = window.viewport_size();
    Xy::new(
        f32::from(viewport.width),
        f32::from(viewport.height) - TOOLBAR_HEIGHT - FOOTER_HEIGHT,
    )
}

impl MainWindow {
    /// Show the recording at `path` on the home page, in place of anything
    /// shown before, paused on its first picture. `size`, its pictures'
    /// size if known, sizes the window before the file is open.
    pub fn show_recording(
        &mut self,
        path: PathBuf,
        size: Option<(u32, u32)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(shown) = self.shown.take() {
            shown.release(window);
        }
        // A new capture: the cleared one can no longer come back.
        if let Some((cleared, _)) = self.cleared.take() {
            cleared.release(window);
        }
        if let Some(video) = self.video.take() {
            video.release(window);
        }
        self.hand = None;
        self.flyout = None;
        self.videos += 1;
        let id = self.videos;
        let mut link = (self.hooks.open_recording)(&path);
        let updates = link.updates.take();
        let (volume, muted) = {
            let settings = self.settings();
            (settings.playback_volume.min(100), settings.playback_muted)
        };
        link.send(Command::Volume(f64::from(volume) / 100.));
        link.send(Command::Mute(muted));
        let seek = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(SEEK_RANGE)
                .step(0.01)
                .default_value(0.)
        });
        let volume_slider = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(100.)
                .step(1.)
                .default_value(volume as f32)
        });
        let sliders = [
            cx.subscribe(&seek, |this, _, event: &SliderEvent, cx| {
                this.on_seek_slider(event, cx)
            }),
            cx.subscribe(&volume_slider, |this, _, event: &SliderEvent, cx| {
                this.on_volume_slider(event, cx)
            }),
        ];
        let task = cx.spawn_in(window, async move |this, cx| {
            let Some(mut updates) = updates else {
                return;
            };
            while let Some(update) = updates.next().await {
                let applied = this.update_in(cx, |this, window, cx| {
                    this.on_playback(id, update, window, cx)
                });
                if applied.is_err() {
                    break;
                }
            }
        });
        self.video = Some(Video {
            id,
            path,
            link,
            size,
            duration: Duration::ZERO,
            position: Duration::ZERO,
            playing: false,
            frame: None,
            stale: Vec::new(),
            asked: None,
            failed: None,
            seek,
            volume: volume_slider,
            volume_open: false,
            scrubbing: false,
            _updates: task,
            _sliders: sliders,
        });
        self.page = super::Page::Home;
        self.settings = None;
        self.menu = None;
        self.fit_home(window, cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The recording shown, if any.
    pub fn recording(&self) -> Option<&std::path::Path> {
        self.video.as_ref().map(|video| video.path.as_path())
    }

    /// Whether the recording shown is playing.
    pub fn is_playing(&self) -> bool {
        self.video.as_ref().is_some_and(Video::is_playing)
    }

    /// Close the recording shown, back to the start view.
    pub(super) fn close_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(video) = self.video.take() else {
            return false;
        };
        video.release(window);
        self.fit_home(window, cx);
        cx.notify();
        true
    }

    fn on_playback(
        &mut self,
        id: u64,
        update: Update,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(video) = self.video.as_mut().filter(|video| video.id == id) else {
            return;
        };
        match update {
            Update::Loaded {
                width,
                height,
                duration,
            } => {
                video.duration = duration;
                let resized = video.size != Some((width, height));
                video.size = Some((width, height));
                if resized {
                    self.fit_home(window, cx);
                }
            }
            Update::Frame {
                bgra,
                width,
                height,
            } => {
                let image = pixels::bgra_image(bgra, width, height);
                if let Some(old) = video.frame.replace(image) {
                    video.stale.push(old);
                }
            }
            Update::Position(at) => {
                video.position = at;
                if !video.scrubbing {
                    let value = video.value_at(at);
                    video
                        .seek
                        .update(cx, |slider, cx| slider.set_value(value, window, cx));
                }
            }
            Update::Playing(playing) => video.playing = playing,
            Update::Ended => video.playing = false,
            Update::Failed(why) => video.failed = Some(why),
        }
        cx.notify();
    }

    fn on_seek_slider(&mut self, event: &SliderEvent, cx: &mut Context<Self>) {
        let Some(video) = self.video.as_mut() else {
            return;
        };
        let (value, done) = match event {
            SliderEvent::Change(value) => (value.end(), false),
            SliderEvent::Release(value) => (value.end(), true),
        };
        let at = video.time_at(value);
        video.scrubbing = !done;
        video.position = at;
        video.link.send(Command::Seek(at));
        cx.notify();
    }

    fn on_volume_slider(&mut self, event: &SliderEvent, cx: &mut Context<Self>) {
        let (value, done) = match event {
            SliderEvent::Change(value) => (value.end(), false),
            SliderEvent::Release(value) => (value.end(), true),
        };
        let volume = value.round().clamp(0., 100.) as u32;
        if done {
            // Saved once the drag ends, not at each step of it.
            self.set_volume(volume, None, cx);
        } else if let Some(video) = &self.video {
            video.link.send(Command::Volume(f64::from(volume) / 100.));
            // Turning it up unmutes, as Windows' own slider does.
            if self.settings().playback_muted {
                video.link.send(Command::Mute(false));
            }
        }
    }

    /// Set the volume, percent, and whether it is muted (as it was, if
    /// `None`; turning the volume up unmutes), and remember both.
    fn set_volume(&mut self, volume: u32, muted: Option<bool>, cx: &mut Context<Self>) {
        let volume = volume.min(100);
        let was = {
            let settings = self.settings();
            (settings.playback_volume, settings.playback_muted)
        };
        let muted = muted.unwrap_or(was.1 && volume <= was.0);
        self.change(cx, |s| {
            s.playback_volume = volume;
            s.playback_muted = muted;
        });
        if let Some(video) = &self.video {
            video.link.send(Command::Volume(f64::from(volume) / 100.));
            video.link.send(Command::Mute(muted));
        }
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        let Some(video) = &self.video else {
            return;
        };
        video.link.send(if video.playing {
            Command::Pause
        } else {
            Command::Play
        });
        cx.notify();
    }

    /// Go to `at`, clamped to the recording.
    fn seek_to(&mut self, at: Duration, window: &mut Window, cx: &mut Context<Self>) {
        let Some(video) = self.video.as_mut() else {
            return;
        };
        let at = at.min(video.duration);
        video.position = at;
        video.link.send(Command::Seek(at));
        let value = video.value_at(at);
        video
            .seek
            .update(cx, |slider, cx| slider.set_value(value, window, cx));
        cx.notify();
    }

    fn toggle_volume(&mut self, cx: &mut Context<Self>) {
        if let Some(video) = &mut self.video {
            video.volume_open = !video.volume_open;
            cx.notify();
        }
    }

    /// Close the volume's slider. Returns whether it was open.
    pub(super) fn close_volume(&mut self) -> bool {
        self.video
            .as_mut()
            .is_some_and(|video| std::mem::take(&mut video.volume_open))
    }

    /// A key for the recording shown. Returns whether it was one.
    pub(super) fn on_video_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(video) = &self.video else {
            return false;
        };
        let (position, duration) = (video.position, video.duration);
        let volume = self.settings().playback_volume;
        match key {
            "space" => self.toggle_play(cx),
            "left" => self.seek_to(position.saturating_sub(SKIP), window, cx),
            "right" => self.seek_to(position + SKIP, window, cx),
            "home" => self.seek_to(Duration::ZERO, window, cx),
            "end" => self.seek_to(duration, window, cx),
            "up" | "down" => {
                let volume = if key == "up" {
                    volume + VOLUME_STEP
                } else {
                    volume.saturating_sub(VOLUME_STEP)
                };
                self.set_volume(volume, None, cx);
                self.sync_volume_slider(window, cx);
            }
            "m" if self.single_keys() => {
                let muted = self.settings().playback_muted;
                self.set_volume(volume, Some(!muted), cx);
            }
            "escape" if self.close_volume() => cx.notify(),
            _ => return false,
        }
        true
    }

    /// Move the volume's slider to the volume set.
    fn sync_volume_slider(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(video) = &self.video {
            let volume = self.settings().playback_volume as f32;
            video
                .volume
                .update(cx, |slider, cx| slider.set_value(volume, window, cx));
        }
    }

    /// The recording, fitted and centred, with the play bar over its
    /// bottom; or why it cannot be played.
    pub(super) fn video_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let area = video_area(window);
        let scale = window.scale_factor();
        let Some(video) = self.video.as_mut() else {
            return div().into_any_element();
        };
        let shown = video.size.map(|size| fitted(size, area, scale));
        // Pictures at the size shown, physical pixels.
        if let Some(shown) = shown {
            let ask = (
                (shown.x * scale).round().max(1.) as u32,
                (shown.y * scale).round().max(1.) as u32,
            );
            if video.asked != Some(ask) {
                video.asked = Some(ask);
                video.link.send(Command::Size(ask.0, ask.1));
            }
        }
        let picture = shown.zip(video.frame.clone()).map(|(shown, frame)| {
            div()
                .absolute()
                .left(px((area.x - shown.x) / 2. - 1.))
                .top(px((area.y - shown.y) / 2. - 1.))
                .border_1()
                .border_color(border())
                .child(img(frame).w(px(shown.x)).h(px(shown.y)))
        });
        let failed = video.failed.clone().map(|why| {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1p5()
                .child("This recording can't be played.")
                .child(div().text_xs().text_color(muted()).child(why))
        });
        let bar = video.failed.is_none().then(|| {
            div()
                .absolute()
                .bottom(px(BAR_GAP))
                .left(px(0.))
                .right(px(0.))
                .flex()
                .justify_center()
                .child(self.play_bar((area.x - 2. * BAR_GAP).min(BAR_WIDTH), cx))
        });
        div()
            .id("video")
            .role(Role::Group)
            .aria_label("Recording")
            .test_support()
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .children(picture)
            .children(failed)
            .children(bar)
            .into_any_element()
    }

    fn play_bar(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let Some(video) = &self.video else {
            return div().into_any_element();
        };
        let time = |text: String| {
            div()
                .flex_none()
                .min_w(px(56.))
                .text_sm()
                .text_color(gpui_kit::white())
                .child(text)
        };
        div()
            .id("play-bar")
            .role(Role::Toolbar)
            .aria_label("Playback")
            .test_support()
            .relative()
            .flex()
            .items_center()
            .gap_3()
            .w(px(width))
            .h(px(BAR_HEIGHT))
            .px_3()
            .rounded_lg()
            .bg(rgb(0x2C2C2C))
            .border_1()
            .border_color(border())
            .shadow_lg()
            .occlude()
            .child(bar_button(
                "play",
                if video.playing { "Pause" } else { "Play" },
                if video.playing {
                    IconName::Pause
                } else {
                    IconName::Play
                },
                |d| d.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_play(cx))),
            ))
            .child(time(clock(video.position)))
            .child(
                div()
                    .flex_1()
                    .child(Slider::new(&video.seek).bg(coral()).text_color(coral())),
            )
            .child(time(clock(video.duration)))
            .child(self.volume_button(cx))
            .into_any_element()
    }

    fn volume_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (volume, muted) = {
            let settings = self.settings();
            (settings.playback_volume, settings.playback_muted)
        };
        let open = self.video.as_ref().is_some_and(|video| video.volume_open);
        div()
            .relative()
            .child(bar_button(
                "volume",
                "Volume",
                volume_icon(volume, muted),
                |d| {
                    d.when(open, |d| d.bg(hover()))
                        // Opening on the press, which then stops: the
                        // window's own press handler closes flyouts.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                this.toggle_volume(cx);
                            }),
                        )
                },
            ))
            .when(open, |d| {
                d.child(deferred(self.volume_flyout(volume, muted, cx)).with_priority(1))
            })
    }

    /// The volume's slider, above its button: a button to mute, the slider
    /// and the volume, as in Snipping Tool.
    fn volume_flyout(&self, volume: u32, silent: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(video) = &self.video else {
            return div().into_any_element();
        };
        div()
            .id("volume-flyout")
            .role(Role::Dialog)
            .aria_label("Volume")
            .test_support()
            .absolute()
            .bottom(px(BAR_HEIGHT - 4.))
            .right(px(-12.))
            .flex()
            .items_center()
            .gap_3()
            .w(px(300.))
            .h(px(56.))
            .px_3()
            .rounded_lg()
            .bg(rgb(0x2C2C2C))
            .border_1()
            .border_color(border())
            .shadow_lg()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(bar_button(
                "mute",
                if silent { "Unmute" } else { "Mute" },
                volume_icon(volume, silent),
                |d| {
                    d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_volume(volume, Some(!silent), cx)
                    }))
                },
            ))
            .child(
                div()
                    .flex_1()
                    .child(Slider::new(&video.volume).bg(coral()).text_color(coral())),
            )
            .child(
                div()
                    .flex_none()
                    .min_w(px(28.))
                    .text_sm()
                    .text_color(if silent { muted() } else { gpui_kit::white() })
                    .child(volume.to_string()),
            )
            .into_any_element()
    }
}

/// Volume's icon: crossed out when muted or at zero, with fewer waves when
/// low.
fn volume_icon(volume: u32, muted: bool) -> IconName {
    match volume {
        _ if muted => IconName::VolumeX,
        0 => IconName::VolumeX,
        1..50 => IconName::Volume1,
        _ => IconName::Volume2,
    }
}

/// A square button on the play bar, which `with` gives its handlers.
fn bar_button(
    id: &'static str,
    label: &'static str,
    icon: IconName,
    with: impl FnOnce(Stateful<Div>) -> Stateful<Div>,
) -> impl IntoElement {
    let button = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(40.))
        .rounded_md()
        .hover(|s| s.bg(hover()))
        .cursor_pointer()
        .child(Icon::new(icon).size(px(20.)));
    with(button)
        .role(Role::Button)
        .aria_label(label)
        .test_support()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_as_hours_minutes_and_seconds() {
        assert_eq!(clock(Duration::ZERO), "0:00:00");
        assert_eq!(clock(Duration::from_millis(61_900)), "0:01:01");
        assert_eq!(clock(Duration::from_secs(3 * 3600 + 5)), "3:00:05");
    }

    #[test]
    fn a_recording_fits_the_area_but_is_never_enlarged() {
        // 4K on a 1.5× screen, in a 1300 × 700 area: the height decides.
        let shown = fitted((3840, 2160), Xy::new(1300., 700.), 1.5);
        assert!((shown.y - (700. - 2. * MARGIN)).abs() < 0.01);
        assert!((shown.x / shown.y - 16. / 9.).abs() < 0.01);
        // A small one keeps its own size.
        let shown = fitted((300, 200), Xy::new(1300., 700.), 1.0);
        assert_eq!((shown.x, shown.y), (300., 200.));
    }

    #[test]
    fn the_volume_icon_shows_the_level() {
        assert_eq!(volume_icon(80, true), IconName::VolumeX);
        assert_eq!(volume_icon(0, false), IconName::VolumeX);
        assert_eq!(volume_icon(30, false), IconName::Volume1);
        assert_eq!(volume_icon(100, false), IconName::Volume2);
    }
}
