//! Shuttercrab's window, after Snipping Tool's: a toolbar to start a capture
//! (New, Screenshot or Record, the target, the delay) and a ⋯ menu with
//! Settings, the folder and Quit. Settings open as a page of the same
//! window, so Shuttercrab only ever has one window on the taskbar.
//!
//! Everything has a key: N or Enter starts a capture, S and R pick the
//! mode, A, W, D and F the target, T steps through the delays, comma opens
//! Settings, O the folder, Ctrl+Q quits. Escape closes a menu, or goes back
//! from Settings.
//!
//! The window reaches the rest of Shuttercrab only through [`MainHooks`],
//! so it can be tested on its own.

use crate::{
    capture_bar::{CaptureMode, CaptureTarget, muted, recording, surface, tile},
    settings::{COUNTDOWN_CHOICES, DELAY_CHOICES, Settings},
    settings_window::{Hooks, SettingsWindow},
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Hsla, InteractiveElement as _, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseUpEvent, ParentElement as _, Pixels, Render,
    Role, SharedString, Size, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    Window,
    assets::IconName,
    component::{Icon, Theme, ThemeMode},
    deferred, div,
    prelude::FluentBuilder as _,
    px, rgb,
};
use std::rc::Rc;

/// The window's size on each page, logical pixels.
pub const HOME_SIZE: Size<Pixels> = Size {
    width: px(760.),
    height: px(300.),
};
pub const SETTINGS_SIZE: Size<Pixels> = Size {
    width: px(880.),
    height: px(690.),
};

/// Starts a capture of a mode and target.
pub type CaptureHook = Rc<dyn Fn(CaptureMode, CaptureTarget, &mut Window, &mut App)>;

/// What the window needs from the rest of Shuttercrab.
pub struct MainHooks {
    /// The settings page's hooks, and through them the live settings.
    pub settings: Rc<Hooks>,
    /// Start a capture: `mode` of `target`, after the delay the settings
    /// name. The app hides the window meanwhile.
    pub capture: CaptureHook,
    pub open_folder: Rc<dyn Fn(&mut App)>,
    pub quit: Rc<dyn Fn(&mut App)>,
}

/// What the window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    Settings,
}

/// A menu open below its toolbar button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu {
    Delay,
    More,
}

/// The ⋯ menu's items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum More {
    Settings,
    OpenFolder,
    Quit,
}

impl More {
    const ALL: [More; 3] = [More::Settings, More::OpenFolder, More::Quit];

    fn label(self) -> &'static str {
        match self {
            More::Settings => "Settings",
            More::OpenFolder => "Open screenshots folder",
            More::Quit => "Quit Shuttercrab",
        }
    }

    fn key(self) -> &'static str {
        match self {
            More::Settings => ",",
            More::OpenFolder => "O",
            More::Quit => "Ctrl+Q",
        }
    }

    fn icon(self) -> IconName {
        match self {
            More::Settings => IconName::Settings,
            More::OpenFolder => IconName::FolderOpen,
            More::Quit => IconName::Power,
        }
    }
}

/// Shuttercrab's coral, the main window's accent.
fn coral() -> Hsla {
    rgb(0xE8603C).into()
}

fn border() -> Hsla {
    rgb(0x3A3A3A).into()
}

fn hover() -> Hsla {
    rgb(0x353535).into()
}

/// Give the theme the window's own colours, so the Settings page matches
/// the rest of it. Dark always, like the Capture Bar and the overlay.
pub fn apply_theme(window: &mut Window, cx: &mut App) {
    Theme::change(ThemeMode::Dark, Some(window), cx);
    Theme::update(cx, |theme| {
        let colors = &mut theme.colors;
        colors.background = surface();
        colors.sidebar = surface();
        colors.sidebar_accent = tile();
        colors.border = border();
        colors.sidebar_border = border();
        colors.popover = rgb(0x2C2C2C).into();
    });
}

fn seconds(s: u32) -> String {
    if s == 0 {
        "Off".into()
    } else {
        format!("{s} seconds")
    }
}

pub struct MainWindow {
    hooks: Rc<MainHooks>,
    page: Page,
    settings: Option<Entity<SettingsWindow>>,
    menu: Option<Menu>,
    /// The highlighted item of the open menu.
    highlighted: usize,
    focus: FocusHandle,
}

impl MainWindow {
    pub fn new(hooks: Rc<MainHooks>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            hooks,
            page: Page::Home,
            settings: None,
            menu: None,
            highlighted: 0,
            focus,
        }
    }

    pub fn page(&self) -> Page {
        self.page
    }

    /// Show `page`, sizing the window to it.
    pub fn show(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if page == self.page && (page == Page::Home || self.settings.is_some()) {
            return;
        }
        self.page = page;
        match page {
            Page::Home => {
                self.settings = None;
                window.resize(HOME_SIZE);
                window.focus(&self.focus, cx);
            }
            Page::Settings => {
                let hooks = self.hooks.settings.clone();
                let main = cx.entity().downgrade();
                self.settings = Some(cx.new(|cx| {
                    SettingsWindow::new(hooks, window, cx).on_escape(move |window, cx| {
                        let _ = main.update(cx, |this, cx| this.show(Page::Home, window, cx));
                    })
                }));
                window.resize(SETTINGS_SIZE);
            }
        }
        cx.notify();
    }

    fn settings(&self) -> std::cell::Ref<'_, Settings> {
        self.hooks.settings.settings.borrow()
    }

    fn change(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut Settings)) {
        f(&mut self.hooks.settings.settings.borrow_mut());
        (self.hooks.settings.changed)(cx);
        cx.notify();
    }

    fn mode(&self) -> CaptureMode {
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
        }
    }

    fn set_delay(&mut self, seconds: u32, cx: &mut Context<Self>) {
        let mode = self.mode();
        self.change(cx, |s| match mode {
            CaptureMode::Screenshot => s.screenshot_delay = seconds,
            CaptureMode::Record => s.recording_countdown = seconds,
        });
    }

    fn step_delay(&mut self, cx: &mut Context<Self>) {
        let (now, choices) = self.delay();
        let at = choices.iter().position(|s| *s == now).unwrap_or(0);
        self.set_delay(choices[(at + 1) % choices.len()], cx);
    }

    fn set_mode(&mut self, mode: CaptureMode, cx: &mut Context<Self>) {
        self.change(cx, |s| s.last_mode = mode);
    }

    fn set_target(&mut self, target: CaptureTarget, cx: &mut Context<Self>) {
        if self.mode().offers(target) {
            self.change(cx, |s| s.last_target = target);
        }
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
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
        };
        cx.notify();
    }

    fn menu_len(&self, menu: Menu) -> usize {
        match menu {
            Menu::Delay => self.delay().1.len(),
            Menu::More => More::ALL.len(),
        }
    }

    fn choose_more(&mut self, item: More, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        match item {
            More::Settings => self.show(Page::Settings, window, cx),
            More::OpenFolder => (self.hooks.open_folder)(cx),
            More::Quit => (self.hooks.quit)(cx),
        }
        cx.notify();
    }

    fn choose_highlighted(&mut self, menu: Menu, window: &mut Window, cx: &mut Context<Self>) {
        match menu {
            Menu::Delay => {
                let choices = self.delay().1;
                self.menu = None;
                self.set_delay(choices[self.highlighted.min(choices.len() - 1)], cx);
            }
            Menu::More => self.choose_more(More::ALL[self.highlighted], window, cx),
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.page != Page::Home {
            return;
        }
        let keystroke = &event.keystroke;
        let key = keystroke.key.as_str();
        if keystroke.modifiers.control {
            if key == "q" {
                (self.hooks.quit)(cx);
            }
            return;
        }
        if keystroke.modifiers.alt || keystroke.modifiers.platform {
            return;
        }
        if let Some(menu) = self.menu {
            let len = self.menu_len(menu);
            match key {
                "escape" => self.menu = None,
                "up" => self.highlighted = (self.highlighted + len - 1) % len,
                "down" | "tab" => self.highlighted = (self.highlighted + 1) % len,
                "enter" | "space" => return self.choose_highlighted(menu, window, cx),
                _ => {}
            }
            if matches!(key, "escape" | "up" | "down" | "tab") {
                cx.notify();
                return;
            }
        }
        match key {
            "n" | "enter" => self.start(window, cx),
            "s" => self.set_mode(CaptureMode::Screenshot, cx),
            "r" => self.set_mode(CaptureMode::Record, cx),
            "t" => self.step_delay(cx),
            "," => self.choose_more(More::Settings, window, cx),
            "o" => self.choose_more(More::OpenFolder, window, cx),
            _ => {
                if let Some(target) = CaptureTarget::ALL.into_iter().find(|t| t.key() == key) {
                    self.set_target(target, cx);
                }
            }
        }
    }

    /// A small "key" hint: a letter in capitals, anything longer as written.
    fn key_hint(text: &str) -> impl IntoElement + use<> {
        let text = if text.len() == 1 {
            text.to_uppercase()
        } else {
            text.to_string()
        };
        div().text_xs().text_color(muted()).child(text)
    }

    fn new_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
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
            .hover(|s| s.bg(hover()))
            .cursor_pointer()
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| this.start(window, cx)),
            )
            .child(Icon::new(IconName::Plus).size(px(16.)).text_color(coral()))
            .child("New")
            .child(Self::key_hint("n"))
    }

    /// One item of a segmented control: an icon and its key, underlined in
    /// coral when chosen, dimmed when not `offered`.
    #[allow(clippy::too_many_arguments)]
    fn segment<F: Fn(&mut Self, &mut Context<Self>) + 'static>(
        id: &'static str,
        label: &'static str,
        icon: IconName,
        key: &'static str,
        on: bool,
        offered: bool,
        icon_color: Option<Hsla>,
        cx: &mut Context<Self>,
        choose: F,
    ) -> impl IntoElement + use<F> {
        div()
            .id(id)
            .role(Role::RadioButton)
            .aria_label(label)
            .test_support()
            .relative()
            .flex()
            .items_center()
            .gap_1p5()
            .h(px(34.))
            .px_2()
            .rounded_md()
            .when_else(
                on,
                |d| d.bg(tile()).text_color(gpui_kit::white()),
                |d| d.text_color(muted()),
            )
            .when(!offered, |d| d.opacity(0.35))
            .when(offered && !on, |d| {
                d.hover(|s| s.text_color(gpui_kit::white()))
                    .cursor_pointer()
            })
            .when(offered, |d| {
                d.on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseUpEvent, _, cx| choose(this, cx)),
                )
            })
            .child(
                Icon::new(icon)
                    .size(px(18.))
                    .when_some(icon_color.filter(|_| on), |i, c| i.text_color(c)),
            )
            .child(Self::key_hint(key))
            .when(on, |d| {
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

    fn modes(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mode = self.mode();
        Self::segmented([
            Self::segment(
                "mode-screenshot",
                "Screenshot",
                IconName::Camera,
                "s",
                mode == CaptureMode::Screenshot,
                true,
                None,
                cx,
                |this, cx| this.set_mode(CaptureMode::Screenshot, cx),
            )
            .into_any_element(),
            Self::segment(
                "mode-record",
                "Record",
                IconName::Video,
                "r",
                mode == CaptureMode::Record,
                true,
                Some(recording()),
                cx,
                |this, cx| this.set_mode(CaptureMode::Record, cx),
            )
            .into_any_element(),
        ])
    }

    fn targets(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (mode, chosen) = (self.mode(), self.target());
        Self::segmented(CaptureTarget::ALL.map(|target| {
            Self::segment(
                target.id(),
                target.label(),
                target.icon(),
                target.key(),
                target == chosen,
                mode.offers(target),
                None,
                cx,
                move |this, cx| this.set_target(target, cx),
            )
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

    fn menu_list(&self, menu: Menu, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let items: Vec<(String, String, Option<IconName>, bool)> = match menu {
            Menu::Delay => {
                let (now, choices) = self.delay();
                choices
                    .iter()
                    .map(|s| (seconds(*s), String::new(), None, *s == now))
                    .collect()
            }
            Menu::More => More::ALL
                .iter()
                .map(|m| (m.label().into(), m.key().into(), Some(m.icon()), false))
                .collect(),
        };
        let right = menu == Menu::More;
        div()
            .id(match menu {
                Menu::Delay => "delay-menu",
                Menu::More => "more-menu",
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
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .children(
                items
                    .into_iter()
                    .enumerate()
                    .map(|(i, (label, key, icon, chosen))| {
                        let highlighted = i == self.highlighted;
                        div()
                            .id(SharedString::from(format!(
                                "{}-{i}",
                                match menu {
                                    Menu::Delay => "delay",
                                    Menu::More => "more",
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
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseUpEvent, window, cx| {
                                    this.highlighted = i;
                                    this.choose_highlighted(menu, window, cx);
                                }),
                            )
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
                            .child(Self::key_hint(&key))
                    }),
            )
    }

    fn delay_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (now, _) = self.delay();
        let label = match self.mode() {
            CaptureMode::Screenshot => format!("Delay: {}", seconds(now)),
            CaptureMode::Record => format!("Countdown: {}", seconds(now)),
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
            .child(Self::key_hint("t"));
        self.menu_button("delay", label, Menu::Delay, content, cx)
    }

    fn more_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let content = Icon::new(IconName::Ellipsis).size(px(20.));
        self.menu_button("more", "More".into(), Menu::More, content, cx)
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .id("toolbar")
            .role(Role::Toolbar)
            .test_support()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_3()
            .border_b_1()
            .border_color(border())
            .child(self.new_button(cx))
            .child(self.modes(cx))
            .child(div().w(px(1.)).h(px(28.)).bg(border()))
            .child(self.targets(cx))
            .child(self.delay_button(cx))
            .child(div().flex_1())
            .child(self.more_button(cx))
    }

    fn hint(&self) -> impl IntoElement + use<> {
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
                    .child("Or choose above and press N"),
            )
    }

    fn settings_page(
        &self,
        settings: Entity<SettingsWindow>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(50.))
                    .px_3()
                    .border_b_1()
                    .border_color(border())
                    .child(
                        div()
                            .id("back")
                            .role(Role::Button)
                            .aria_label("Back")
                            .test_support()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(34.))
                            .rounded_md()
                            .hover(|s| s.bg(hover()))
                            .cursor_pointer()
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseUpEvent, window, cx| {
                                    this.show(Page::Home, window, cx)
                                }),
                            )
                            .child(Icon::new(IconName::ArrowLeft).size(px(18.))),
                    )
                    .child(div().text_sm().child("Settings"))
                    .child(div().text_xs().text_color(muted()).child("Esc to go back")),
            )
            .child(div().flex_1().min_h_0().child(settings))
    }
}

impl Render for MainWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div()
            .id("main-window")
            .role(Role::Pane)
            .aria_label("Shuttercrab")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(surface())
            .text_color(gpui_kit::white())
            .text_sm()
            .on_key_down(cx.listener(Self::on_key_down))
            // A press anywhere else closes an open menu.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    if this.menu.take().is_some() {
                        cx.notify();
                    }
                }),
            );
        match (self.page, self.settings.clone()) {
            (Page::Settings, Some(settings)) => root.child(self.settings_page(settings, cx)),
            _ => root.child(self.toolbar(cx)).child(self.hint()),
        }
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
