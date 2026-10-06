//! Shuttercrab's window, after Snipping Tool's: a toolbar to start a capture
//! (New, Screenshot or Record, the target, the delay) and a ⋯ menu with
//! Settings, the folder and Quit. Settings open as a page of the same
//! window, so Shuttercrab only ever has one window on the taskbar.
//!
//! Like Snipping Tool, it shows a screenshot after it is taken: one started
//! from the window, or one whose thumbnail was clicked. The window sizes
//! itself to show it at full size where the screen allows, the toolbar
//! gains the zoom, Copy and Save as, and the ⋯ menu Edit in Paint (E) and
//! Open with; its folder item shows a saved screenshot's file.
//!
//! Everything has a key: N or Enter starts a capture, S and R pick the
//! mode, A, W, D and F the target, T steps through the delays, comma opens
//! Settings, O the folder, Ctrl+Q quits; with a screenshot shown, Ctrl+C
//! copies it, Ctrl+S saves it as, Ctrl+plus and Ctrl+minus zoom (so does
//! Ctrl with the scroll wheel, around the pointer), Ctrl+0 fits it to the
//! window and Ctrl+1 shows it at full size. The wheel, or a drag, moves a
//! screenshot bigger than the window. P and H pick up the pen and the
//! highlighter, and pressed again open its colours and size; X picks up
//! the eraser, which takes off whole marks, and pressed again offers to
//! take off every one. G picks up the shapes, whose bar has keys of its own
//! (see `shapes`), and V picks up Select, to change a shape drawn before
//! (see `selection`). Escape puts the tool down. Dragging then draws (Shift
//! for a straight line, a square or a circle), and
//! Space+drag or Ctrl+drag moves the screenshot. [ and ] change the size,
//! Ctrl+Z and Ctrl+Y undo and redo. Escape closes a menu, or goes back
//! from Settings. At the bottom, quietly, the version running; once a newer
//! release is downloaded, Restart to update (U) in its place.
//!
//! The window reaches the rest of Shuttercrab only through [`MainHooks`],
//! so it can be tested on its own.

mod canvas;
mod crop;
mod emoji;
mod selection;
mod shapes;
mod tools;

use crate::{
    capture_choice::{CaptureMode, CaptureTarget},
    markup::{Mark, Region, Tool},
    palette::{border, coral, hover, muted, recording, surface, tile},
    settings::{COUNTDOWN_CHOICES, DELAY_CHOICES, Settings},
    settings_window::{Hooks, SettingsWindow},
    shot_view::{self, Xy},
};
use canvas::{Shown, canvas_size, pointer_in_canvas};
use chrono::NaiveDateTime;
use gpui_kit::{
    Animation, AnimationExt as _, App, AppContext as _, ClickEvent, Context, Entity, FocusHandle,
    Hsla, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement as _, Pixels, Render, RenderImage, Role, SharedString, Size,
    StatefulInteractiveElement as _, Styled as _, Task, TestSupportExt as _, Window,
    assets::IconName,
    component::{Icon, Theme, ThemeMode},
    deferred, div,
    prelude::FluentBuilder as _,
    px, rgb,
};
use std::{path::PathBuf, rc::Rc, sync::Arc, time::Duration};
use tools::Flyout;
pub use tools::Hand;

/// The window's size on each page, logical pixels.
pub const HOME_SIZE: Size<Pixels> = Size {
    width: px(760.),
    height: px(300.),
};
pub const SETTINGS_SIZE: Size<Pixels> = Size {
    width: px(880.),
    height: px(690.),
};

/// The window's least width with a screenshot shown, logical pixels: room
/// for the toolbar's drawing tools, zoom, Copy and Save as too.
pub const SHOT_MIN_WIDTH: f32 = 1250.;

/// The toolbar's and the footer's heights, logical pixels: fixed, so the
/// window can be sized around a screenshot.
const TOOLBAR_HEIGHT: f32 = 59.;
const FOOTER_HEIGHT: f32 = 32.;

/// Starts a capture of a mode and target.
pub type CaptureHook = Rc<dyn Fn(CaptureMode, CaptureTarget, &mut Window, &mut App)>;

/// Acts on the screenshot shown.
pub type ShotHook = Rc<dyn Fn(&Shot, &mut App)>;

/// Copies the screenshot shown, resolving to whether it was copied.
pub type CopyHook = Rc<dyn Fn(&Shot, &mut App) -> Task<bool>>;

/// How long Copy shows its check mark after a copy.
const COPIED_FOR: Duration = Duration::from_millis(1500);

/// Opens the screenshots folder, given the screenshot shown, if any.
pub type FolderHook = Rc<dyn Fn(Option<&Shot>, &mut App)>;

/// A screenshot the window shows.
#[derive(Clone)]
pub struct Shot {
    /// Its pixels, as GPUI draws them.
    pub image: Arc<RenderImage>,
    /// The same image as a PNG file, for saving.
    pub png: Arc<Vec<u8>>,
    /// When it was taken, which names its file.
    pub taken_at: NaiveDateTime,
    /// Its file in the screenshots folder, if it was saved there.
    pub saved: Option<PathBuf>,
    /// The scale of the screen it was taken on, if known: pen sizes are in
    /// that screen's logical pixels.
    pub scale: Option<f32>,
    /// The marks drawn on it, which copying, saving and opening it
    /// elsewhere include. `image` and `png` stay as they were taken.
    pub marks: Vec<Mark>,
    /// The part kept by a crop, if cropped, which copying, saving and
    /// opening it elsewhere give.
    pub crop: Option<Region>,
}

impl Shot {
    /// Its size, physical pixels.
    pub fn size(&self) -> (u32, u32) {
        let size = self.image.size(0);
        (size.width.0 as u32, size.height.0 as u32)
    }

    /// The size copying and saving it gives: the crop's, if cropped.
    pub fn output_size(&self) -> (u32, u32) {
        match self.crop {
            Some(crop) => (crop.width, crop.height),
            None => self.size(),
        }
    }

    /// Whether it was marked up or cropped: then it is not the file taken.
    pub fn is_edited(&self) -> bool {
        !self.marks.is_empty() || self.crop.is_some()
    }
}

/// What the window needs from the rest of Shuttercrab.
pub struct MainHooks {
    /// The settings page's hooks, and through them the live settings.
    pub settings: Rc<Hooks>,
    /// Start a capture: `mode` of `target`, after the delay the settings
    /// name. The app hides the window meanwhile.
    pub capture: CaptureHook,
    /// Open the screenshots folder, with the screenshot shown selected if
    /// it was saved there.
    pub open_folder: FolderHook,
    pub quit: Rc<dyn Fn(&mut App)>,
    /// A downloaded release's version, waiting for a restart, if any.
    pub update_ready: Rc<dyn Fn() -> Option<String>>,
    /// Quit so the downloaded release is applied, and start again.
    pub restart_to_update: Rc<dyn Fn(&mut App)>,
    /// Copy the screenshot shown to the clipboard; resolves to whether it
    /// was copied.
    pub copy: CopyHook,
    /// Ask where to save the screenshot shown, and save it there.
    pub save_as: ShotHook,
    /// Open the screenshot shown in Paint.
    pub edit_in_paint: ShotHook,
    /// Ask which program should open the screenshot shown, and open it.
    pub open_with: ShotHook,
    /// Size the window's client area to this many physical pixels, as far
    /// as its monitor allows.
    pub fit_window: FitHook,
}

/// Sizes the window's client area.
pub type FitHook = Rc<dyn Fn(&mut Window, &mut App, Fit)>;

/// The client area the window wants, physical pixels, and the least it
/// can do with: the app may make it smaller to keep it a comfortable share
/// of the screen, but not below that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fit {
    pub width: u32,
    pub height: u32,
    pub least_width: u32,
    pub least_height: u32,
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
    /// With a screenshot shown, as the rest below.
    EditInPaint,
    OpenWith,
    Quit,
}

impl More {
    /// The items, with or without a screenshot shown.
    fn items(shot: bool) -> &'static [More] {
        if shot {
            &[
                More::Settings,
                More::OpenFolder,
                More::EditInPaint,
                More::OpenWith,
                More::Quit,
            ]
        } else {
            &[More::Settings, More::OpenFolder, More::Quit]
        }
    }

    /// `saved`: a screenshot shown, saved in the folder, which opens with
    /// it selected.
    fn label(self, saved: bool) -> &'static str {
        match self {
            More::Settings => "Settings",
            More::OpenFolder if saved => "Show in folder",
            More::OpenFolder => "Open screenshots folder",
            More::EditInPaint => "Edit in Paint",
            More::OpenWith => "Open with…",
            More::Quit => "Quit Shuttercrab",
        }
    }

    fn key(self) -> &'static str {
        match self {
            More::Settings => ",",
            More::OpenFolder => "O",
            More::EditInPaint => "E",
            More::OpenWith => "",
            More::Quit => "Ctrl+Q",
        }
    }

    fn icon(self) -> IconName {
        match self {
            More::Settings => IconName::Settings,
            More::OpenFolder => IconName::FolderOpen,
            More::EditInPaint => IconName::Brush,
            More::OpenWith => IconName::AppWindow,
            More::Quit => IconName::Power,
        }
    }
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
    /// The screenshot shown on the home page, if any.
    shown: Option<Shown>,
    /// Copy shows its check mark: the screenshot was just copied.
    copied: bool,
    /// Counts copies, so the check mark's timer knows whether a later copy
    /// has started its own.
    copies: u64,
    /// The tool in hand, if any: dragging on the screenshot draws or erases
    /// with it rather than moving the screenshot.
    hand: Option<Hand>,
    /// The tool in hand's colours and size, if open.
    flyout: Option<Flyout>,
    /// The eraser's flyout is open.
    eraser_menu: bool,
    /// The Shapes bar's Fill or Outline menu, if open.
    shape_menu: Option<shapes::ShapeMenu>,
    /// The right-click menu on a shape, if open.
    shape_context: Option<selection::ShapeContext>,
    /// The Shapes bar's emoji picker, if open, with the emoji the arrow
    /// keys are on.
    emoji_menu: Option<usize>,
    /// The emoji's art for the picker, drawn the first time it opens.
    emoji_art: std::cell::OnceCell<Vec<Arc<RenderImage>>>,
    /// A short message over the screenshot, such as that every mark was
    /// taken off, and a count for its timer, as for the size label.
    notice: Option<&'static str>,
    notices: u64,
    /// Where the pointer is over the canvas, if it is, as of the last
    /// render: the tool's tip is outlined there.
    pointer: Option<Xy>,
    /// The tool's size just changed from the keyboard: the tip shows it,
    /// labelled, for a moment.
    size_note: bool,
    /// Counts size changes, so the label's timer knows whether a later one
    /// has started its own.
    size_notes: u64,
    /// Space is held: dragging moves the screenshot, whatever the tool.
    space_held: bool,
    /// The pointer is over the screenshot itself, not a bar or a menu on
    /// it: where the window shows the pointers GPUI lacks.
    canvas_hovered: bool,
    focus: FocusHandle,
}

impl MainWindow {
    pub fn new(hooks: Rc<MainHooks>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        crop::bind_keys(cx);
        Self {
            hooks,
            page: Page::Home,
            settings: None,
            menu: None,
            highlighted: 0,
            shown: None,
            copied: false,
            copies: 0,
            hand: None,
            flyout: None,
            eraser_menu: false,
            shape_menu: None,
            shape_context: None,
            emoji_menu: None,
            emoji_art: std::cell::OnceCell::new(),
            notice: None,
            notices: 0,
            pointer: None,
            size_note: false,
            size_notes: 0,
            space_held: false,
            canvas_hovered: false,
            focus,
        }
    }

    pub fn page(&self) -> Page {
        self.page
    }

    /// The screenshot shown, as it was taken, if any.
    pub fn shot(&self) -> Option<&Shot> {
        self.shown.as_ref().map(|shown| &shown.shot)
    }

    /// The screenshot shown with the marks drawn on it so far, if any: what
    /// Copy, Save as and opening it elsewhere act on.
    pub fn marked_shot(&self) -> Option<Shot> {
        self.shown.as_ref().map(Shown::marked)
    }

    /// The drawing tool in hand, if any.
    pub fn tool(&self) -> Option<Tool> {
        self.hand.and_then(Hand::drawing)
    }

    /// The tool in hand, the eraser included, if any.
    pub fn hand(&self) -> Option<Hand> {
        self.hand
    }

    /// Show `shot` on the home page, in place of any shown before, and size
    /// the window around it.
    pub fn show_shot(&mut self, shot: Shot, window: &mut Window, cx: &mut Context<Self>) {
        let shown = Shown::new(shot, window.scale_factor());
        if let Some(previous) = self.shown.replace(shown) {
            previous.release(window);
        }
        self.page = Page::Home;
        self.settings = None;
        self.menu = None;
        self.fit_home(window, cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Size the window for the home page: the toolbar and the hint, or the
    /// screenshot shown at full size.
    fn fit_home(&self, window: &mut Window, cx: &mut App) {
        let Some(shot) = self.shot() else {
            window.resize(HOME_SIZE);
            return;
        };
        let scale = window.scale_factor();
        let (width, height) = shot.size();
        let least = (SHOT_MIN_WIDTH, f32::from(HOME_SIZE.height));
        // A screenshot pixel per screen pixel, plus the margin on each side.
        let around = 2. * shot_view::MARGIN;
        let width = (width as f32 / scale + around).max(least.0);
        let height = (height as f32 / scale + around + TOOLBAR_HEIGHT + FOOTER_HEIGHT).max(least.1);
        let physical = |logical: f32| (logical * scale).ceil() as u32;
        let fit = Fit {
            width: physical(width),
            height: physical(height),
            least_width: physical(least.0),
            least_height: physical(least.1),
        };
        (self.hooks.fit_window)(window, cx, fit);
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
                self.fit_home(window, cx);
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

    /// Copy the screenshot shown; once it is on the clipboard, Copy shows a
    /// check mark for a moment, as Snipping Tool's does.
    fn copy(&mut self, cx: &mut Context<Self>) {
        let Some(shot) = self.marked_shot() else {
            return;
        };
        let done = (self.hooks.copy)(&shot, cx);
        cx.spawn(async move |this, cx| {
            if !done.await {
                return;
            }
            let Ok(copy) = this.update(cx, |this, cx| {
                this.copies += 1;
                this.copied = true;
                cx.notify();
                this.copies
            }) else {
                return;
            };
            cx.background_executor().timer(COPIED_FOR).await;
            // Unless a later copy is showing its own.
            let _ = this.update(cx, |this, cx| {
                if this.copies == copy {
                    this.copied = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn save_as(&mut self, cx: &mut Context<Self>) {
        self.with_shot(cx, |hooks| &hooks.save_as);
    }

    /// Call the hook `which` picks with the screenshot shown, if any.
    fn with_shot(&self, cx: &mut App, which: impl FnOnce(&MainHooks) -> &ShotHook) {
        if let Some(shot) = self.marked_shot() {
            which(&self.hooks)(&shot, cx);
        }
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
            Menu::More => More::items(self.shown.is_some()).len(),
        }
    }

    fn choose_more(&mut self, item: More, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        match item {
            More::Settings => self.show(Page::Settings, window, cx),
            More::OpenFolder => (self.hooks.open_folder)(self.shot(), cx),
            More::EditInPaint => self.with_shot(cx, |hooks| &hooks.edit_in_paint),
            More::OpenWith => self.with_shot(cx, |hooks| &hooks.open_with),
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
            Menu::More => {
                let items = More::items(self.shown.is_some());
                self.choose_more(items[self.highlighted.min(items.len() - 1)], window, cx)
            }
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.page != Page::Home {
            return;
        }
        let keystroke = &event.keystroke;
        let key = keystroke.key.as_str();
        if self.on_crop_key(keystroke, cx) {
            return;
        }
        if keystroke.modifiers.control {
            match key {
                "q" => (self.hooks.quit)(cx),
                "=" | "+" => self.zoom(window, cx, |view, canvas| view.step(1, None, canvas)),
                "-" => self.zoom(window, cx, |view, canvas| view.step(-1, None, canvas)),
                "0" => self.zoom(window, cx, |view, _| view.fit_to_canvas()),
                "1" => self.zoom(window, cx, |view, canvas| view.zoom_to(1., None, canvas)),
                "c" => self.copy(cx),
                "s" => self.save_as(cx),
                "z" if keystroke.modifiers.shift => self.redo(window, cx),
                "z" => self.undo(window, cx),
                "y" => self.redo(window, cx),
                _ => {}
            }
            return;
        }
        if self.on_selection_key(keystroke, window, cx) {
            return;
        }
        if keystroke.modifiers.alt || keystroke.modifiers.platform {
            return;
        }
        if self.on_flyout_key(key, window, cx) || self.on_shapes_key(key, window, cx) {
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
            "escape" => self.put_down_tool(window, cx),
            "space" => {
                self.space_held = true;
                // The pointer shows a hand at once, where it can move.
                cx.notify();
            }
            // The rest are single keys, which the settings can turn off.
            _ if !self.single_keys() => {}
            "p" => self.take(Hand::Draw(Tool::Pen), window, cx),
            "h" => self.take(Hand::Draw(Tool::Highlighter), window, cx),
            "x" => self.take(Hand::Erase, window, cx),
            "g" => self.take(Hand::Shape, window, cx),
            "v" => self.take(Hand::Select, window, cx),
            "c" => self.start_crop(window, cx),
            "[" => self.step_size(-1., window, cx),
            "]" => self.step_size(1., window, cx),
            "n" | "enter" => self.start(window, cx),
            "s" => self.set_mode(CaptureMode::Screenshot, cx),
            "r" => self.set_mode(CaptureMode::Record, cx),
            "t" => self.step_delay(cx),
            "," => self.choose_more(More::Settings, window, cx),
            "o" => self.choose_more(More::OpenFolder, window, cx),
            "e" => self.choose_more(More::EditInPaint, window, cx),
            "u" if (self.hooks.update_ready)().is_some() => (self.hooks.restart_to_update)(cx),
            _ => {
                if let Some(target) = CaptureTarget::ALL.into_iter().find(|t| t.key() == key) {
                    self.set_target(target, cx);
                }
            }
        }
    }

    /// Whether single keys work (N for New, P for the pen, and so on): the
    /// settings can turn them off, against accidental presses.
    pub(super) fn single_keys(&self) -> bool {
        self.settings().single_key_shortcuts
    }

    /// `key`'s hint as shown: nothing for a single key while those are off.
    pub(super) fn shown_key(&self, key: &'static str) -> &'static str {
        if self.single_keys() || key.contains('+') {
            key
        } else {
            ""
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
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.start(window, cx)))
            .child(Icon::new(IconName::Plus).size(px(16.)).text_color(coral()))
            .child("New")
            .child(Self::key_hint(self.shown_key("n")))
    }

    /// One item of a segmented control: an icon and its key, underlined in
    /// coral when chosen, dimmed when not `offered`.
    #[expect(
        clippy::too_many_arguments,
        reason = "each is one part of the segment, named at the call; a struct would only repeat the names"
    )]
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
                d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| choose(this, cx)))
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
                self.shown_key("s"),
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
                self.shown_key("r"),
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
                self.shown_key(target.key()),
                target == chosen,
                mode.offers(target),
                None,
                cx,
                move |this, cx| this.set_target(target, cx),
            )
        }))
    }

    /// A toolbar button for the screenshot shown: an icon and its key.
    fn shot_button(
        id: &'static str,
        label: &'static str,
        icon: IconName,
        key: &'static str,
        cx: &mut Context<Self>,
        action: fn(&mut Self, &mut Context<Self>),
    ) -> impl IntoElement + use<> {
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
            .hover(|s| s.bg(hover()))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| action(this, cx)))
            .child(Icon::new(icon).size(px(18.)))
            .child(Self::key_hint(key))
    }

    /// Copy, which turns into a check mark and "Copied", fading in, for a
    /// moment after each copy.
    fn copy_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (label, icon, key) = if self.copied {
            ("Copied", IconName::Check, "Copied")
        } else {
            ("Copy", IconName::Copy, "Ctrl+C")
        };
        let content = div()
            .flex()
            .items_center()
            .gap_1p5()
            .child(Icon::new(icon).size(px(18.)))
            .child(Self::key_hint(key));
        div()
            .id("copy")
            .role(Role::Button)
            .aria_label(label)
            .test_support()
            .flex()
            .items_center()
            .h(px(40.))
            .px_2p5()
            .rounded_md()
            .hover(|s| s.bg(hover()))
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.copy(cx)))
            .map(|d| {
                if self.copied {
                    // A new id per copy, so each one fades in afresh.
                    d.child(content.with_animation(
                        SharedString::from(format!("copied-{}", self.copies)),
                        Animation::new(Duration::from_millis(200)),
                        |content, delta| content.opacity(delta),
                    ))
                    .into_any_element()
                } else {
                    d.child(content).into_any_element()
                }
            })
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
            Menu::More => {
                let saved = self.shot().is_some_and(|shot| shot.saved.is_some());
                More::items(self.shown.is_some())
                    .iter()
                    .map(|m| {
                        let key = self.shown_key(m.key());
                        (m.label(saved).into(), key.into(), Some(m.icon()), false)
                    })
                    .collect()
            }
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
            .occlude()
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
            .child(Self::key_hint(self.shown_key("t")));
        self.menu_button("delay", label, Menu::Delay, content, cx)
    }

    fn more_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let content = Icon::new(IconName::Ellipsis).size(px(20.));
        self.menu_button("more", "More".into(), Menu::More, content, cx)
    }

    /// `zoom`: the screenshot's zoom, as a percentage, if one is shown.
    fn toolbar(&self, zoom: Option<u32>, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .id("toolbar")
            .role(Role::Toolbar)
            .test_support()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(TOOLBAR_HEIGHT))
            .px_3()
            .border_b_1()
            .border_color(border())
            .child(self.new_button(cx))
            .child(self.modes(cx))
            .child(div().w(px(1.)).h(px(28.)).bg(border()))
            .child(self.targets(cx))
            .child(self.delay_button(cx))
            // Crop mode has its own bar over the screenshot instead.
            .when(self.shown.is_some() && !self.is_cropping(), |d| {
                d.child(self.drawing_tools(cx))
            })
            .child(div().flex_1())
            .when_some(zoom, |d, zoom| d.child(Self::zoom_button(zoom, cx)))
            .when(self.shown.is_some() && !self.is_cropping(), |d| {
                d.child(self.copy_button(cx)).child(Self::shot_button(
                    "save-as",
                    "Save as",
                    IconName::Save,
                    "Ctrl+S",
                    cx,
                    Self::save_as,
                ))
            })
            .child(self.more_button(cx))
    }

    /// The zoom, as a percentage; a click switches between fitting the
    /// window and full size, like Ctrl+0 and Ctrl+1.
    fn zoom_button(zoom: u32, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let label = SharedString::from(format!("{zoom}%"));
        div()
            .id("zoom")
            .role(Role::Button)
            .aria_label(label.clone())
            .test_support()
            .flex()
            .items_center()
            .gap_1p5()
            .h(px(40.))
            .px_2p5()
            .rounded_md()
            .hover(|s| s.bg(hover()))
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.zoom(window, cx, |view, canvas| {
                    if view.is_fitted() {
                        view.zoom_to(1., None, canvas);
                    } else {
                        view.fit_to_canvas();
                    }
                })
            }))
            .child(Icon::new(IconName::ZoomIn).size(px(18.)))
            .child(div().text_xs().child(label))
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
                    .child(if self.single_keys() {
                        "Or choose above and press N"
                    } else {
                        "Or choose above and click New"
                    }),
            )
    }

    /// Which build is running, quietly, so an update is easy to confirm; in
    /// its place, once a newer release is downloaded, a way to restart into
    /// it.
    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let footer = div()
            .flex()
            .flex_none()
            .justify_center()
            .items_start()
            .h(px(FOOTER_HEIGHT))
            .text_xs();
        match (self.hooks.update_ready)() {
            Some(version) => {
                let label = SharedString::from(format!("Restart to update to {version}"));
                footer.child(
                    div()
                        .id("update")
                        .role(Role::Button)
                        .aria_label(label.clone())
                        .test_support()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .border_1()
                        .border_color(coral().opacity(0.5))
                        .text_color(coral())
                        .hover(|s| s.bg(hover()))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            (this.hooks.restart_to_update)(cx)
                        }))
                        .child(Icon::new(IconName::RotateCcw).size(px(12.)))
                        .child(label)
                        .child(Self::key_hint(self.shown_key("u"))),
                )
            }
            None => {
                let version =
                    SharedString::from(format!("Shuttercrab {}", env!("CARGO_PKG_VERSION")));
                footer.child(
                    div()
                        .id("version")
                        .aria_label(version.clone())
                        .test_support()
                        .text_color(muted())
                        .child(version),
                )
            }
        }
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
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.show(Page::Home, window, cx)
                            }))
                            .child(Icon::new(IconName::ArrowLeft).size(px(18.))),
                    )
                    .child(div().text_sm().child("Settings"))
                    .child(div().text_xs().text_color(muted()).child("Esc to go back")),
            )
            .child(div().flex_1().min_h_0().child(settings))
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Moved to a screen of another scale: the screenshot's full size
        // in logical pixels changed, so fit it again.
        if let Some(shown) = &mut self.shown
            && shown.scale != window.scale_factor()
        {
            shown.rescale(window.scale_factor());
        }
        // Cropped, cropping, or a crop undone: fit the part shown.
        if let Some(shown) = &mut self.shown {
            shown.refit();
        }
        if let Some(shown) = &self.shown {
            shown.drop_stale_pictures(window);
        }
        // No screenshot to point at: none of the window's own pointers.
        if self.page != Page::Home || self.shown.is_none() {
            canvas::show_pointer(window, None);
        }
        self.pointer = pointer_in_canvas(window).filter(|_| self.shown.is_some());
        let zoom = self
            .shown
            .as_ref()
            .map(|shown| (shown.view.zoom(canvas_size(window)) * 100.).round() as u32);
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
            .on_key_up(cx.listener(Self::on_key_up))
            .when(self.is_cropping(), |d| {
                d.key_context(crop::CONTEXT)
                    .on_action(cx.listener(|this, _: &crop::NextGrip, _, cx| {
                        this.step_crop_focus(false, cx)
                    }))
                    .on_action(cx.listener(|this, _: &crop::PreviousGrip, _, cx| {
                        this.step_crop_focus(true, cx)
                    }))
            })
            // A press anywhere else closes an open menu.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    if this.menu.take().is_some() | this.close_flyouts() {
                        cx.notify();
                    }
                }),
            );
        match (self.page, self.settings.clone()) {
            (Page::Settings, Some(settings)) => root.child(self.settings_page(settings, cx)),
            _ => root
                .child(self.toolbar(zoom, cx))
                .map(|d| match &self.shown {
                    Some(shown) => d.child(self.canvas(shown, window, cx)),
                    None => d.child(self.hint()),
                })
                .child(self.footer(cx)),
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
