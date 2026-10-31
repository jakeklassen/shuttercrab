//! The home page's controls: New, the mode, the target and the delay,
//! Watch, and the ⋯ menu, with the menus they open; and the middle of the
//! window while nothing is shown.

use super::{ButtonFace, Cleared, MainWindow, Page};
use crate::{
    capture_choice::{CaptureMode, CaptureTarget, WATCH_KEY},
    settings::{COUNTDOWN_CHOICES, DELAY_CHOICES},
    ui::palette::{border, coral, hover, muted, recording, tile},
};
use gpui_kit::{
    ClickEvent, Context, Hsla, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    ParentElement as _, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, assets::IconName, component::Icon, deferred, div,
    prelude::FluentBuilder as _, px, rgb,
};

/// A menu open below its toolbar button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Menu {
    Delay,
    More,
    /// The targets, folded into one menu in a narrow window.
    Target,
}

/// The ⋯ menu's items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum More {
    Settings,
    OpenFolder,
    /// With a screenshot shown, as the rest below: in the OS's image editor.
    EditIn,
    OpenWith,
    Quit,
}

impl More {
    /// `saved`: a screenshot shown, saved in the folder, which opens with
    /// it selected. `editor`: the image editor Edit in opens.
    fn label(self, saved: bool, editor: &str) -> String {
        match self {
            More::Settings => "Settings".into(),
            More::OpenFolder if saved => "Show in folder".into(),
            More::OpenFolder => "Open screenshots folder".into(),
            More::EditIn => format!("Edit in {editor}"),
            More::OpenWith => "Open with…".into(),
            More::Quit => "Quit Shuttercrab".into(),
        }
    }

    fn key(self) -> &'static str {
        match self {
            More::Settings => ",",
            More::OpenFolder => "O",
            More::EditIn => "E",
            More::OpenWith => "",
            More::Quit => "Ctrl+Q",
        }
    }

    fn icon(self) -> IconName {
        match self {
            More::Settings => IconName::Settings,
            More::OpenFolder => IconName::FolderOpen,
            More::EditIn => IconName::Brush,
            More::OpenWith => IconName::AppWindow,
            More::Quit => IconName::Power,
        }
    }
}

/// One row of a toolbar menu.
struct MenuRow {
    label: String,
    /// Its key hint, as shown: empty for none.
    key: &'static str,
    icon: Option<IconName>,
    /// The choice in force, marked with a coral bar.
    chosen: bool,
}

/// One item of a segmented control, such as a mode or a target.
struct Segment {
    face: ButtonFace,
    /// The one chosen: lit, and underlined in coral.
    chosen: bool,
    /// Whether it can be chosen now; dimmed when not.
    offered: bool,
    /// Its icon's colour while chosen, if not the text's.
    chosen_color: Option<Hsla>,
}

fn seconds(s: u32) -> String {
    if s == 0 {
        "Off".into()
    } else {
        format!("{s} seconds")
    }
}

impl MainWindow {
    pub(super) fn mode(&self) -> CaptureMode {
        self.settings().last_mode
    }

    /// The target to capture: the last one used, or Area when the mode does
    /// not offer it.
    fn target(&self) -> CaptureTarget {
        let settings = self.settings();
        if settings.last_mode.offers(settings.last_target) {
            settings.last_target
        } else {
            CaptureTarget::Area
        }
    }

    /// The delay for the current mode, and the choices it has.
    fn delay(&self) -> (u32, &'static [u32]) {
        let settings = self.settings();
        match settings.last_mode {
            CaptureMode::Screenshot => (settings.delay(), &DELAY_CHOICES),
            CaptureMode::Record => (settings.countdown(), &COUNTDOWN_CHOICES),
            // A stream starts when its bar says Go live.
            CaptureMode::Stream => (0, &[0]),
        }
    }

    fn set_delay(&mut self, seconds: u32, cx: &mut Context<Self>) {
        let mode = self.mode();
        self.change(cx, |s| match mode {
            CaptureMode::Screenshot => s.screenshot_delay = seconds,
            CaptureMode::Record => s.recording_countdown = seconds,
            CaptureMode::Stream => {}
        });
    }

    pub(super) fn step_delay(&mut self, cx: &mut Context<Self>) {
        let (now, choices) = self.delay();
        let at = choices.iter().position(|s| *s == now).unwrap_or(0);
        self.set_delay(choices[(at + 1) % choices.len()], cx);
    }

    pub(super) fn set_mode(&mut self, mode: CaptureMode, cx: &mut Context<Self>) {
        self.change(cx, |s| s.last_mode = mode);
    }

    pub(super) fn set_target(&mut self, target: CaptureTarget, cx: &mut Context<Self>) {
        if self.mode().offers(target) {
            self.change(cx, |s| s.last_target = target);
        }
    }

    pub(super) fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        // New is off until there is a key to stream with; the middle of the
        // window says how to add one.
        if self.needs_host_key() {
            return;
        }
        self.stream_note = None;
        let (mode, target) = (self.mode(), self.target());
        (self.hooks.capture)(mode, target, window, cx);
        cx.notify();
    }

    fn toggle_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        self.menu = (self.menu != Some(menu)).then_some(menu);
        self.highlighted = match menu {
            Menu::Delay => {
                let (now, choices) = self.delay();
                choices.iter().position(|s| *s == now).unwrap_or(0)
            }
            Menu::More => 0,
            Menu::Target => {
                let chosen = self.target();
                self.offered_targets()
                    .iter()
                    .position(|t| *t == chosen)
                    .unwrap_or(0)
            }
        };
        cx.notify();
    }

    /// The targets the mode offers, in order.
    fn offered_targets(&self) -> Vec<CaptureTarget> {
        let mode = self.mode();
        CaptureTarget::ALL
            .into_iter()
            .filter(|t| mode.offers(*t))
            .collect()
    }

    /// The ⋯ menu's items now: more with a screenshot or a recording shown;
    /// Edit in only with an image editor.
    fn more_items(&self) -> Vec<More> {
        let items: &[More] = if self.shown.is_some() {
            &[
                More::Settings,
                More::OpenFolder,
                More::EditIn,
                More::OpenWith,
                More::Quit,
            ]
        } else if self.recording.is_some() {
            &[More::Settings, More::OpenFolder, More::OpenWith, More::Quit]
        } else {
            &[More::Settings, More::OpenFolder, More::Quit]
        };
        let has_editor = self.hooks.image_editor.is_some();
        items
            .iter()
            .copied()
            .filter(|item| has_editor || *item != More::EditIn)
            .collect()
    }

    pub(super) fn menu_len(&self, menu: Menu) -> usize {
        match menu {
            Menu::Delay => self.delay().1.len(),
            Menu::More => self.more_items().len(),
            Menu::Target => self.offered_targets().len(),
        }
    }

    pub(super) fn choose_more(&mut self, item: More, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        match item {
            More::Settings => self.show(Page::Settings, window, cx),
            More::OpenFolder => match &self.recording {
                Some(video) => (self.hooks.reveal)(&video.path, cx),
                None => (self.hooks.open_folder)(self.shot(), cx),
            },
            More::EditIn => self.with_shot(cx, |hooks| &hooks.edit_in_editor),
            More::OpenWith => match &self.recording {
                Some(video) => (self.hooks.open_file_with)(&video.path, cx),
                None => self.with_shot(cx, |hooks| &hooks.open_with),
            },
            More::Quit => (self.hooks.quit)(cx),
        }
        cx.notify();
    }

    pub(super) fn choose_highlighted(
        &mut self,
        menu: Menu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match menu {
            Menu::Delay => {
                let choices = self.delay().1;
                self.menu = None;
                self.set_delay(choices[self.highlighted.min(choices.len() - 1)], cx);
            }
            Menu::More => {
                let items = self.more_items();
                self.choose_more(items[self.highlighted.min(items.len() - 1)], window, cx)
            }
            Menu::Target => {
                let targets = self.offered_targets();
                self.menu = None;
                self.set_target(targets[self.highlighted.min(targets.len() - 1)], cx);
            }
        }
    }

    /// Tab or Shift+Tab in an open menu: the next item, or the one before.
    pub(super) fn step_menu(&mut self, by: isize, cx: &mut Context<Self>) {
        if let Some(menu) = self.menu {
            let len = self.menu_len(menu) as isize;
            self.highlighted = (self.highlighted as isize + by).rem_euclid(len) as usize;
            cx.notify();
        }
    }

    pub(super) fn new_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let off = self.needs_host_key();
        div()
            .id("new")
            .role(Role::Button)
            .aria_label("New capture")
            .test_support()
            .flex()
            .items_center()
            .gap_2()
            .h(px(40.))
            .px_3()
            .rounded_md()
            .bg(tile())
            .border_1()
            .border_color(border())
            .when_else(
                off,
                |d| d.opacity(0.35),
                |d| d.hover(|s| s.bg(hover())).cursor_pointer(),
            )
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.start(window, cx)))
            .child(Icon::new(IconName::Plus).size(px(16.)).text_color(coral()))
            .child("New")
            .children(Self::key_hint(self.shown_key("n")))
    }

    /// One item of a segmented control: an icon and its key, underlined in
    /// coral when chosen, dimmed when not offered.
    fn segment<F: Fn(&mut Self, &mut Context<Self>) + 'static>(
        segment: Segment,
        cx: &mut Context<Self>,
        choose: F,
    ) -> impl IntoElement + use<F> {
        let Segment {
            face,
            chosen,
            offered,
            chosen_color,
        } = segment;
        div()
            .id(face.id)
            .role(Role::RadioButton)
            .aria_label(face.label)
            .test_support()
            .relative()
            .flex()
            .items_center()
            .gap_1p5()
            .h(px(34.))
            .px_2()
            .rounded_md()
            .when_else(
                chosen,
                |d| d.bg(tile()).text_color(gpui_kit::white()),
                |d| d.text_color(muted()),
            )
            .when(!offered, |d| d.opacity(0.35))
            .when(offered && !chosen, |d| {
                d.hover(|s| s.text_color(gpui_kit::white()))
                    .cursor_pointer()
            })
            .when(offered, |d| {
                d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| choose(this, cx)))
            })
            .child(
                Icon::new(face.icon)
                    .size(px(18.))
                    .when_some(chosen_color.filter(|_| chosen), |i, c| i.text_color(c)),
            )
            .children(Self::key_hint(face.key))
            .when(chosen, |d| {
                d.child(
                    div()
                        .absolute()
                        .bottom(px(2.))
                        .left(px(0.))
                        .right(px(0.))
                        .flex()
                        .justify_center()
                        .child(div().w(px(16.)).h(px(3.)).rounded_full().bg(coral())),
                )
            })
    }

    fn segmented(children: impl IntoIterator<Item = impl IntoElement>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(px(2.))
            .p(px(3.))
            .rounded_lg()
            .bg(rgb(0x1A1A1A))
            .border_1()
            .border_color(border())
            .children(children)
    }

    pub(super) fn modes(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let chosen = self.mode();
        let segments: Vec<_> = CaptureMode::ALL
            .iter()
            .map(|&mode| {
                let segment = Segment {
                    face: ButtonFace {
                        id: mode.id(),
                        label: mode.label(),
                        icon: mode.icon(),
                        key: self.shown_key(mode.key()),
                    },
                    chosen: mode == chosen,
                    offered: true,
                    // Recording and streaming show in the recording red.
                    chosen_color: mode.is_video().then(recording),
                };
                Self::segment(segment, cx, move |this, cx| this.set_mode(mode, cx))
            })
            .collect();
        Self::segmented(segments)
    }

    /// Watch someone's stream: opens the Watch box. On the home page only,
    /// where the toolbar has room.
    pub(super) fn watch_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let segment = Segment {
            face: ButtonFace {
                id: "watch",
                label: "Watch a stream",
                icon: IconName::MonitorPlay,
                key: self.shown_key(WATCH_KEY),
            },
            chosen: false,
            offered: true,
            chosen_color: None,
        };
        Self::segmented([Self::segment(segment, cx, |this, cx| {
            (this.hooks.watch.clone())(cx)
        })])
    }

    pub(super) fn targets(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (mode, chosen) = (self.mode(), self.target());
        Self::segmented(CaptureTarget::ALL.map(|target| {
            let segment = Segment {
                face: ButtonFace {
                    id: target.id(),
                    label: target.label(),
                    icon: target.icon(),
                    key: self.shown_key(target.key()),
                },
                chosen: target == chosen,
                offered: mode.offers(target),
                chosen_color: None,
            };
            Self::segment(segment, cx, move |this, cx| this.set_target(target, cx))
        }))
    }

    /// A toolbar button that opens `menu` below it.
    fn menu_button<C: IntoElement>(
        &self,
        id: &'static str,
        label: String,
        menu: Menu,
        content: C,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<C> {
        let open = self.menu == Some(menu);
        div()
            .relative()
            .child(
                div()
                    .id(id)
                    .role(Role::Button)
                    .aria_label(label)
                    .test_support()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .h(px(40.))
                    .px_2p5()
                    .rounded_md()
                    .when(open, |d| d.bg(hover()))
                    .hover(|s| s.bg(hover()))
                    .cursor_pointer()
                    // Opening on the press, which then stops: the window's
                    // own press handler closes menus.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.toggle_menu(menu, cx);
                        }),
                    )
                    .child(content),
            )
            .when(open, |d| {
                d.child(deferred(self.menu_list(menu, cx)).with_priority(1))
            })
    }

    /// A menu's rows.
    fn menu_rows(&self, menu: Menu) -> Vec<MenuRow> {
        match menu {
            Menu::Delay => {
                let (now, choices) = self.delay();
                choices
                    .iter()
                    .map(|s| MenuRow {
                        label: seconds(*s),
                        key: "",
                        icon: None,
                        chosen: *s == now,
                    })
                    .collect()
            }
            Menu::More => {
                let saved = self.shot().is_some_and(|shot| shot.saved.is_some())
                    || self.recording.is_some();
                let editor = self.hooks.image_editor.unwrap_or_default();
                self.more_items()
                    .iter()
                    .map(|m| MenuRow {
                        label: m.label(saved, editor),
                        key: self.shown_key(m.key()),
                        icon: Some(m.icon()),
                        chosen: false,
                    })
                    .collect()
            }
            Menu::Target => {
                let chosen = self.target();
                self.offered_targets()
                    .into_iter()
                    .map(|t| MenuRow {
                        label: t.label().into(),
                        key: self.shown_key(t.key()),
                        icon: Some(t.icon()),
                        chosen: t == chosen,
                    })
                    .collect()
            }
        }
    }

    fn menu_list(&self, menu: Menu, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let rows = self.menu_rows(menu);
        let right = menu == Menu::More;
        div()
            .id(match menu {
                Menu::Delay => "delay-menu",
                Menu::More => "more-menu",
                Menu::Target => "target-menu",
            })
            .role(Role::Menu)
            .test_support()
            .absolute()
            .top(px(46.))
            .when_else(right, |d| d.right(px(0.)), |d| d.left(px(0.)))
            .min_w(px(if right { 260. } else { 170. }))
            .p(px(5.))
            .rounded_lg()
            .bg(rgb(0x2C2C2C))
            .border_1()
            .border_color(border())
            .shadow_lg()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .children(rows.into_iter().enumerate().map(|(i, row)| {
                let MenuRow {
                    label,
                    key,
                    icon,
                    chosen,
                } = row;
                let highlighted = i == self.highlighted;
                div()
                    .id(SharedString::from(format!(
                        "{}-{i}",
                        match menu {
                            Menu::Delay => "delay",
                            Menu::More => "more",
                            Menu::Target => "target-choice",
                        }
                    )))
                    .role(Role::MenuItem)
                    .aria_label(label.clone())
                    .test_support()
                    .relative()
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .h(px(36.))
                    .px_3()
                    .rounded_md()
                    .text_sm()
                    .text_color(gpui_kit::white())
                    .when(highlighted, |d| d.bg(rgb(0x383838)))
                    .hover(|s| s.bg(rgb(0x383838)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.highlighted = i;
                        this.choose_highlighted(menu, window, cx);
                    }))
                    .when(chosen, |d| {
                        d.child(
                            div()
                                .absolute()
                                .left(px(0.))
                                .top(px(10.))
                                .bottom(px(10.))
                                .w(px(3.))
                                .rounded_full()
                                .bg(coral()),
                        )
                    })
                    .when_some(icon, |d, icon| d.child(Icon::new(icon).size(px(16.))))
                    .child(div().flex_1().child(label))
                    .children(Self::key_hint(key))
            }))
    }

    pub(super) fn delay_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (now, _) = self.delay();
        let label = match self.mode() {
            CaptureMode::Screenshot => format!("Delay: {}", seconds(now)),
            CaptureMode::Record => format!("Countdown: {}", seconds(now)),
            CaptureMode::Stream => String::new(),
        };
        let content = div()
            .flex()
            .items_center()
            .gap_1p5()
            .child(
                Icon::new(if now == 0 {
                    IconName::TimerOff
                } else {
                    IconName::Timer
                })
                .size(px(18.)),
            )
            .when(now > 0, |d| {
                d.child(div().text_sm().child(format!("{now}s")))
            })
            .child(Icon::new(IconName::ChevronDown).size(px(14.)))
            .children(Self::key_hint(self.shown_key("t")));
        self.menu_button("delay", label, Menu::Delay, content, cx)
    }

    pub(super) fn more_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let content = Icon::new(IconName::Ellipsis).size(px(20.));
        self.menu_button("more", "More".into(), Menu::More, content, cx)
    }

    pub(super) fn hint(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let back = match self.cleared {
            Some(Cleared::Shot { .. }) => Some("Bring the screenshot back"),
            Some(Cleared::Recording(_)) => Some("Bring the recording back"),
            None => None,
        };
        let (bar, area) = {
            let settings = self.settings();
            (
                settings.capture_bar_hotkey.replace('+', " + "),
                settings.screenshot_hotkey.replace('+', " + "),
            )
        };
        let bold = |text: String| {
            div()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(text)
        };
        if self.mode() == CaptureMode::Stream && back.is_none() {
            return self.stream_hint(cx).into_any_element();
        }
        div()
            .id("hint")
            .test_support()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1p5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child("Press")
                    .child(bold(bar))
                    .child("to start a capture, or")
                    .child(bold(area))
                    .child("for an area"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted())
                    .child(if self.single_keys() {
                        "Or choose above and press N"
                    } else {
                        "Or choose above and click New"
                    }),
            )
            // Just cleared: the way back.
            .when_some(back, |d, back| {
                d.child(
                    div()
                        .id("unclear")
                        .role(Role::Button)
                        .aria_label(format!("{back} (Ctrl+Z)"))
                        .test_support()
                        .mt_2()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .px_2p5()
                        .py_1()
                        .rounded_md()
                        .text_xs()
                        .bg(tile())
                        .hover(|s| s.bg(hover()))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.undo_clear(window, cx);
                        }))
                        .child(Icon::new(IconName::Undo2).size(px(14.)))
                        .child(back)
                        .child(div().text_color(muted()).child("Ctrl+Z")),
                )
            })
            .into_any_element()
    }

    /// In a narrow window: the target chosen, opening the menu of targets.
    pub(super) fn target_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let target = self.target();
        let content = div()
            .flex()
            .items_center()
            .gap_1p5()
            .child(Icon::new(target.icon()).size(px(18.)))
            .child(Icon::new(IconName::ChevronDown).size(px(14.)));
        self.menu_button(
            "target",
            format!("Capture: {}", target.label()),
            Menu::Target,
            content,
            cx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_read_as_words() {
        assert_eq!(seconds(0), "Off");
        assert_eq!(seconds(5), "5 seconds");
    }
}
