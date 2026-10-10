//! The settings (PRD §26): General, Screenshot, Recording and Diagnostics
//! pages, shown as a page of the main window. Every change applies at once;
//! there is no Save button. It all works from the keyboard: Tab between
//! controls, Space or Enter to use them, Escape to go back.
//!
//! The window reaches the rest of Shuttercrab only through [`Hooks`], so it
//! can be tested on its own.

use crate::settings::Settings;
use futures::future::LocalBoxFuture;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, MouseButton, MouseUpEvent, ParentElement as _, PathPromptOptions, Render, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
    component::{
        ActiveTheme as _, Disableable as _,
        button::Button,
        setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings as SettingsUi},
    },
    div,
    prelude::FluentBuilder as _,
    px,
};
use shuttercrab_capture::{MonitorInfo, record::Microphone};
use shuttercrab_platform::{
    Hotkey,
    os::{Os, os},
};
use std::{cell::RefCell, path::PathBuf, rc::Rc};

/// What the settings window needs from the rest of Shuttercrab.
pub struct Hooks {
    /// The live settings, shared with the capture flows.
    pub settings: Rc<RefCell<Settings>>,
    /// After any change: save the settings and refresh the tray menu.
    pub changed: Rc<dyn Fn(&mut App)>,
    /// Unregister the global hotkeys, while a new one is being recorded,
    /// and pass on Print Screen when pressed: windows never see that key go
    /// down.
    pub pause_hotkeys: Rc<dyn Fn(OnPrintScreen)>,
    /// Register the hotkeys the settings name. Resolves to the ids of those
    /// another application already owns.
    pub apply_hotkeys: Rc<dyn Fn() -> LocalBoxFuture<'static, Vec<u32>>>,
    /// Register every hotkey the settings name, including those taken only
    /// while recording, then go back to the ones that apply now. Resolves
    /// to the ids another application already owns.
    pub probe_hotkeys: Rc<dyn Fn() -> LocalBoxFuture<'static, Vec<u32>>>,
    /// Whether the OS keeps Print Screen for its own screenshots.
    pub print_screen_taken: Rc<dyn Fn() -> bool>,
    /// Whether Shuttercrab starts at sign-in, and a way to change it.
    pub launch_at_startup: Rc<dyn Fn() -> bool>,
    pub set_launch_at_startup: Rc<dyn Fn(bool)>,
    /// What the Diagnostics page shows.
    pub diagnostics: Diagnostics,
}

/// Shown by a hotkey field while Windows keeps Print Screen (only Windows
/// does).
const PRINT_SCREEN_HELD: &str =
    "Windows opens Snipping Tool with Print Screen. Turn that off to use it here.";

/// What a recording hotkey field does with a Print Screen hotkey.
pub type OnPrintScreen = Rc<dyn Fn(Hotkey, &mut App)>;

/// Facts about this machine and this copy of Shuttercrab.
#[derive(Clone, Debug)]
pub struct Diagnostics {
    pub version: String,
    pub os: &'static Os,
    pub monitors: Vec<MonitorInfo>,
    pub log_dir: Option<PathBuf>,
    pub settings_path: Option<PathBuf>,
}

/// Which hotkey a recorder edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyKind {
    CaptureBar,
    Screenshot,
    ScreenshotWithWindow,
    Record,
    Pause,
    Restart,
    Discard,
    Undo,
}

impl HotkeyKind {
    pub const ALL: [HotkeyKind; 8] = [
        Self::CaptureBar,
        Self::Screenshot,
        Self::ScreenshotWithWindow,
        Self::Record,
        Self::Pause,
        Self::Restart,
        Self::Discard,
        Self::Undo,
    ];

    fn get(self, s: &Settings) -> &str {
        match self {
            Self::CaptureBar => &s.capture_bar_hotkey,
            Self::Screenshot => &s.screenshot_hotkey,
            Self::ScreenshotWithWindow => &s.screenshot_with_window_hotkey,
            Self::Record => &s.record_hotkey,
            Self::Pause => &s.pause_hotkey,
            Self::Restart => &s.restart_hotkey,
            Self::Discard => &s.discard_hotkey,
            Self::Undo => &s.undo_hotkey,
        }
    }

    fn set(self, s: &mut Settings, value: String) {
        let slot = match self {
            Self::CaptureBar => &mut s.capture_bar_hotkey,
            Self::Screenshot => &mut s.screenshot_hotkey,
            Self::ScreenshotWithWindow => &mut s.screenshot_with_window_hotkey,
            Self::Record => &mut s.record_hotkey,
            Self::Pause => &mut s.pause_hotkey,
            Self::Restart => &mut s.restart_hotkey,
            Self::Discard => &mut s.discard_hotkey,
            Self::Undo => &mut s.undo_hotkey,
        };
        *slot = value;
    }

    /// What the hotkey does, to finish "Ctrl+Alt+S already …".
    fn does(self) -> &'static str {
        match self {
            Self::CaptureBar => "opens the Capture Bar",
            Self::Screenshot => "takes area screenshots",
            Self::ScreenshotWithWindow => "takes area screenshots with this window",
            Self::Record => "starts and stops recording",
            Self::Pause => "pauses recording",
            Self::Restart => "restarts recording",
            Self::Discard => "discards recording",
            Self::Undo => "undoes a discard",
        }
    }

    /// The field's element id.
    fn element_id(self) -> &'static str {
        match self {
            Self::CaptureBar => "hotkey-capture-bar",
            Self::Screenshot => "hotkey-screenshot",
            Self::ScreenshotWithWindow => "hotkey-screenshot-with-window",
            Self::Record => "hotkey-record",
            Self::Pause => "hotkey-pause",
            Self::Restart => "hotkey-restart",
            Self::Discard => "hotkey-discard",
            Self::Undo => "hotkey-undo",
        }
    }

    /// The id the app registers it under.
    fn id(self) -> u32 {
        use crate::app::*;
        match self {
            Self::CaptureBar => CAPTURE_BAR_HOTKEY,
            Self::Screenshot => SCREENSHOT_HOTKEY,
            Self::ScreenshotWithWindow => SCREENSHOT_WITH_WINDOW_HOTKEY,
            Self::Record => RECORD_HOTKEY,
            Self::Pause => PAUSE_HOTKEY,
            Self::Restart => RESTART_HOTKEY,
            Self::Discard => DISCARD_HOTKEY,
            Self::Undo => UNDO_HOTKEY,
        }
    }
}

/// The hotkey a key press makes, as Shuttercrab writes it, or why it cannot
/// be one. `key` is GPUI's key name (`s`, `f5`, `space`).
pub fn hotkey_from_keys(
    ctrl: bool,
    alt: bool,
    shift: bool,
    win: bool,
    key: &str,
) -> Result<String, String> {
    if !(ctrl || alt || win) {
        return Err(
            "Include Ctrl, Alt or Win, so the shortcut does not get in the way of typing, or use Print Screen."
                .into(),
        );
    }
    let mut text = String::new();
    for (on, name) in [
        (ctrl, "Ctrl+"),
        (alt, "Alt+"),
        (shift, "Shift+"),
        (win, "Win+"),
    ] {
        if on {
            text.push_str(name);
        }
    }
    text.push_str(key);
    Hotkey::parse(&text)
        .map(|hotkey| hotkey.to_string())
        .map_err(|_| "Use a letter, a digit, an F key, Space or Print Screen.".into())
}

/// Shows one hotkey; Enter, Space or a click starts recording a new one.
pub struct HotkeyField {
    kind: HotkeyKind,
    hooks: Rc<Hooks>,
    recording: bool,
    message: Option<SharedString>,
    focus: FocusHandle,
}

impl HotkeyField {
    pub fn new(
        kind: HotkeyKind,
        hooks: Rc<Hooks>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle().tab_stop(true);
        // Leaving the field while recording cancels the recording.
        cx.on_blur(&focus, window, |this, _, cx| {
            if this.recording {
                this.stop(cx);
            }
        })
        .detach();
        // Back from Windows' settings, say again whether it keeps Print
        // Screen.
        cx.observe_window_activation(window, |_, _, cx| cx.notify())
            .detach();
        Self {
            kind,
            hooks,
            recording: false,
            message: None,
            focus,
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recording
    }

    /// Whether to say that Windows keeps Print Screen: while a new hotkey
    /// is pressed, or while this one uses Print Screen.
    fn print_screen_held(&self) -> bool {
        let uses_print_screen = self
            .kind
            .get(&self.hooks.settings.borrow())
            .ends_with("PrintScreen");
        (self.recording || uses_print_screen) && (self.hooks.print_screen_taken)()
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.recording = true;
        self.message = None;
        // Otherwise pressing the current hotkey would start a capture.
        let field = cx.entity().downgrade();
        (self.hooks.pause_hotkeys)(Rc::new(move |hotkey, cx| {
            let _ = field.update(cx, |field, cx| {
                if field.recording {
                    field.record(hotkey.to_string(), cx);
                }
            });
        }));
        cx.notify();
    }

    /// Stop recording, keeping the hotkey as it was, and register the
    /// hotkeys the settings name. A refusal's message goes with it.
    fn stop(&mut self, cx: &mut Context<Self>) {
        self.recording = false;
        self.message = None;
        cx.notify();
        let applied = (self.hooks.apply_hotkeys)();
        cx.spawn(async move |_, _| {
            applied.await;
        })
        .detach();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if !self.recording {
            if matches!(keystroke.key.as_str(), "enter" | "space") {
                cx.stop_propagation();
                self.start(cx);
            }
            return;
        }
        cx.stop_propagation();
        if keystroke.key == "escape" {
            self.stop(cx);
            return;
        }
        let m = &keystroke.modifiers;
        match hotkey_from_keys(m.control, m.alt, m.shift, m.platform, &keystroke.key) {
            Ok(hotkey) => self.record(hotkey, cx),
            Err(message) => {
                self.message = Some(message.into());
                cx.notify();
            }
        }
    }

    /// Make `hotkey` this field's, unless another hotkey or application
    /// has it.
    fn record(&mut self, hotkey: String, cx: &mut Context<Self>) {
        let taken_by = HotkeyKind::ALL
            .into_iter()
            .filter(|k| *k != self.kind)
            .find(|k| k.get(&self.hooks.settings.borrow()) == hotkey);
        if let Some(other) = taken_by {
            self.message = Some(format!("{hotkey} already {}.", other.does()).into());
            cx.notify();
            return;
        }
        let previous = self.kind.get(&self.hooks.settings.borrow()).to_string();
        self.kind
            .set(&mut self.hooks.settings.borrow_mut(), hotkey.clone());
        self.recording = false;
        cx.notify();
        // Every hotkey, including those taken only while recording, so a
        // clash shows now rather than mid-recording.
        let (applied, id, kind) = ((self.hooks.probe_hotkeys)(), self.kind.id(), self.kind);
        cx.spawn(async move |this, cx| {
            let taken = applied.await;
            let _ = this.update(cx, |this, cx| {
                if taken.contains(&id) {
                    // Keep the old one; another application owns the new.
                    kind.set(&mut this.hooks.settings.borrow_mut(), previous);
                    this.message = Some(format!("{hotkey} is used by another application.").into());
                    let restored = (this.hooks.apply_hotkeys)();
                    cx.spawn(async move |_, _| {
                        restored.await;
                    })
                    .detach();
                } else {
                    this.message = None;
                    (this.hooks.changed)(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for HotkeyField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let theme = cx.theme();
        let (border, muted, danger, bg) = (
            if focused || self.recording {
                theme.ring
            } else {
                theme.border
            },
            theme.muted_foreground,
            theme.danger,
            theme.background,
        );
        let text: SharedString = if self.recording {
            "Press the new shortcut… (Esc to cancel)".into()
        } else {
            self.kind
                .get(&self.hooks.settings.borrow())
                .to_string()
                .into()
        };
        let id = self.kind.element_id();
        div()
            .flex()
            .flex_col()
            .items_end()
            .gap_1()
            .child(
                div()
                    .id(id)
                    .role(Role::Button)
                    .aria_label(text.clone())
                    .test_support()
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(Self::on_key_down))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, window, cx| {
                            window.focus(&this.focus, cx);
                            if !this.recording {
                                this.start(cx);
                            }
                        }),
                    )
                    .min_w(px(200.))
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .bg(bg)
                    .cursor_pointer()
                    .text_sm()
                    .when(self.recording, |d| d.text_color(muted))
                    .child(text),
            )
            .when(self.print_screen_held(), |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id(SharedString::from(format!("{id}-print-screen")))
                                .test_support()
                                .max_w(px(260.))
                                .text_xs()
                                .text_color(muted)
                                .child(PRINT_SCREEN_HELD),
                        )
                        .child(
                            Button::new(SharedString::from(format!("{id}-print-screen-setting")))
                                .label("Open setting")
                                .on_click(|_, _, cx| {
                                    if let Some(page) = os().print_screen_setting {
                                        cx.open_url(page);
                                    }
                                }),
                        ),
                )
            })
            .when_some(self.message.clone(), |d, message| {
                d.child(
                    div()
                        .id(SharedString::from(format!("{id}-message")))
                        .role(Role::Alert)
                        .aria_label(message.clone())
                        .test_support()
                        .max_w(px(320.))
                        .text_xs()
                        .text_color(danger)
                        .child(message),
                )
            })
    }
}

/// What Escape does in place of closing.
type Escape = Rc<dyn Fn(&mut Window, &mut App)>;

/// The settings window's content.
pub struct SettingsWindow {
    hooks: Rc<Hooks>,
    /// A field for every hotkey, in [`HotkeyKind::ALL`] order.
    hotkeys: Vec<Entity<HotkeyField>>,
    focus: FocusHandle,
    /// What Escape does instead of closing the window, when the settings
    /// are a page of a larger window.
    on_escape: Option<Escape>,
    /// The microphones plugged in when the settings opened.
    microphones: Vec<Microphone>,
}

impl SettingsWindow {
    pub fn new(hooks: Rc<Hooks>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let hotkeys = HotkeyKind::ALL
            .map(|kind| cx.new(|cx| HotkeyField::new(kind, hooks.clone(), window, cx)))
            .to_vec();
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let microphones = shuttercrab_capture::record::microphones()
            .inspect_err(|e| log::warn!("could not list the microphones: {e:#}"))
            .unwrap_or_default();
        Self {
            hooks,
            hotkeys,
            focus,
            on_escape: None,
            microphones,
        }
    }

    /// Escape calls `f` rather than closing the window.
    pub fn on_escape(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_escape = Some(Rc::new(f));
        self
    }

    /// The field for `kind`, as a setting.
    fn hotkey(&self, kind: HotkeyKind) -> SettingField<SharedString> {
        let index = HotkeyKind::ALL.iter().position(|k| *k == kind).unwrap_or(0);
        let field = self.hotkeys[index].clone();
        SettingField::render(move |_, _, _| field.clone())
    }

    /// A switch bound to one boolean setting.
    fn switch(
        &self,
        get: fn(&Settings) -> bool,
        set: fn(&mut Settings, bool),
    ) -> SettingField<bool> {
        let (read, write) = (self.hooks.clone(), self.hooks.clone());
        SettingField::switch(
            move |_| get(&read.settings.borrow()),
            move |value, cx| {
                set(&mut write.settings.borrow_mut(), value);
                (write.changed)(cx);
            },
        )
    }

    fn general(&self) -> SettingPage {
        let hooks = self.hooks.clone();
        let (startup_get, startup_set) = (hooks.clone(), hooks.clone());
        SettingPage::new("General")
            .default_open(true)
            .group(
                SettingGroup::new().item(heading("Startup", None)).item(
                    SettingItem::new(
                        "Start Shuttercrab when you sign in",
                        SettingField::switch(
                            move |_| (startup_get.launch_at_startup)(),
                            move |value, _| (startup_set.set_launch_at_startup)(value),
                        ),
                    )
                    .description("It starts in the tray, without opening this window."),
                ),
            )
            .group(
                SettingGroup::new()
                    .item(heading(
                        "Hotkeys",
                        Some("Select a shortcut and press Enter (or click it), then press the new keys."),
                    ))
                    .item(SettingItem::new(
                        "Open the Capture Bar",
                        self.hotkey(HotkeyKind::CaptureBar),
                    ))
                    .item(
                        SettingItem::new(
                            "Screenshot an area",
                            self.hotkey(HotkeyKind::Screenshot),
                        )
                        .description("Shuttercrab's window hides first."),
                    )
                    .item(
                        SettingItem::new(
                            "Screenshot an area with this window",
                            self.hotkey(HotkeyKind::ScreenshotWithWindow),
                        )
                        .description("Shuttercrab's window stays in the picture."),
                    )
                    .item(
                        SettingItem::new("Record an area", self.hotkey(HotkeyKind::Record))
                            .description("Press it again to stop."),
                    )
                    .item(
                        SettingItem::new(
                            "Single-key shortcuts in this window",
                            self.switch(
                                |s| s.single_key_shortcuts,
                                |s, v| s.single_key_shortcuts = v,
                            ),
                        )
                        .description(
                            "N for New, P for the pen and the like. Off, only Ctrl shortcuts and Escape work, so a stray key press does nothing.",
                        ),
                    ),
            )
    }

    fn screenshot(&self) -> SettingPage {
        let (read, write) = (self.hooks.clone(), self.hooks.clone());
        let folder = self.hooks.clone();
        let durations = [3u32, 6, 10, 20]
            .map(|s| {
                (
                    SharedString::from(s.to_string()),
                    SharedString::from(format!("{s} seconds")),
                )
            })
            .to_vec();
        SettingPage::new("Screenshot")
            .group(
                SettingGroup::new()
                    .item(heading("Capture", None))
                    .item(
                        SettingItem::new(
                            "Include the pointer",
                            self.switch(|s| s.include_cursor, |s, v| s.include_cursor = v),
                        )
                        .description("Where the pointer was when you pressed the hotkey."),
                    )
                    .item(
                        SettingItem::new(
                            "Snap to window edges",
                            self.switch(|s| s.snap_to_windows, |s, v| s.snap_to_windows = v),
                        )
                        .description("Area selections hold to nearby window edges."),
                    ),
            )
            .group(
                SettingGroup::new()
                    .item(heading("Where screenshots go", None))
                    .item(SettingItem::new(
                        "Copy to the clipboard",
                        self.switch(|s| s.copy_to_clipboard, |s, v| s.copy_to_clipboard = v),
                    ))
                    .item(
                        SettingItem::new(
                            "Save to the folder",
                            self.switch(|s| s.auto_save, |s, v| s.auto_save = v),
                        )
                        .description("When off, screenshots go to the clipboard only."),
                    )
                    .item(SettingItem::new(
                        "Folder",
                        SettingField::render(move |_, _, _| {
                            folder_row(folder.clone(), Folder::Screenshots)
                        }),
                    )),
            )
            .group(
                SettingGroup::new()
                    .item(heading("After a capture", None))
                    .item(SettingItem::new(
                        "Show screenshots in Shuttercrab's window",
                        self.switch(|s| s.show_in_window, |s, v| s.show_in_window = v),
                    ))
                    .item(SettingItem::new(
                        "Show a thumbnail",
                        self.switch(|s| s.show_thumbnail, |s, v| s.show_thumbnail = v),
                    ))
                    .item(SettingItem::new(
                        "Thumbnail stays for",
                        SettingField::dropdown(
                            durations,
                            move |_| read.settings.borrow().thumbnail_seconds.to_string().into(),
                            move |value, cx| {
                                if let Ok(seconds) = value.parse() {
                                    write.settings.borrow_mut().thumbnail_seconds = seconds;
                                    (write.changed)(cx);
                                }
                            },
                        ),
                    ))
                    .item(
                        SettingItem::new(
                            "Show a notification",
                            self.switch(
                                |s| s.notify_after_capture,
                                |s, v| s.notify_after_capture = v,
                            ),
                        )
                        .description(format!("A {} notification with the file name.", os().name)),
                    ),
            )
    }

    /// A dropdown of `choices` (value, label) bound to one setting.
    fn dropdown(
        &self,
        choices: Vec<(u32, String)>,
        get: fn(&Settings) -> u32,
        set: fn(&mut Settings, u32),
    ) -> SettingField<SharedString> {
        let (read, write) = (self.hooks.clone(), self.hooks.clone());
        let choices = choices
            .into_iter()
            .map(|(value, label)| (SharedString::from(value.to_string()), label.into()))
            .collect();
        SettingField::dropdown(
            choices,
            move |_| get(&read.settings.borrow()).to_string().into(),
            move |value, cx| {
                if let Ok(value) = value.parse() {
                    set(&mut write.settings.borrow_mut(), value);
                    (write.changed)(cx);
                }
            },
        )
    }

    /// Where recordings go, and what follows when one is saved.
    fn recording_files(&self) -> SettingGroup {
        let folder = self.hooks.clone();
        SettingGroup::new()
            .item(heading("Where recordings go", None))
            .item(SettingItem::new(
                "Folder",
                SettingField::render(move |_, _, _| folder_row(folder.clone(), Folder::Recordings)),
            ))
            .item(
                SettingItem::new(
                    "Open in Shuttercrab's window",
                    self.switch(
                        |s| s.show_recordings_in_window,
                        |s, v| s.show_recordings_in_window = v,
                    ),
                )
                .description("When a recording is saved, ready to play."),
            )
            .item(
                SettingItem::new(
                    "Show a notification",
                    self.switch(
                        |s| s.notify_after_recording,
                        |s, v| s.notify_after_recording = v,
                    ),
                )
                .description("When a recording is saved; click it to play the file."),
            )
    }

    /// The Recording page's sound: all off unless switched on.
    fn sound(&self) -> SettingGroup {
        SettingGroup::new()
            .item(heading(
                "Sound",
                Some("Off unless switched on here, so nothing is recorded by surprise."),
            ))
            .item(
                SettingItem::new(
                    "System sound",
                    self.switch(|s| s.record_system_sound, |s, v| s.record_system_sound = v),
                )
                .description("What your speakers or headphones play."),
            )
            .item(
                SettingItem::new(
                    "In window recordings, only that app's sound",
                    self.switch(
                        |s| s.record_app_sound_only,
                        |s, v| s.record_app_sound_only = v,
                    ),
                )
                .description(
                    "Leaves out music, calls and notifications. B switches it while recording.",
                ),
            )
            .item(SettingItem::new(
                "Microphone",
                self.switch(|s| s.record_microphone, |s, v| s.record_microphone = v),
            ))
            .item(
                SettingItem::new("Which microphone", self.microphone_choice()).description(
                    format!(
                        "{} follows the one set in {}.",
                        os().default_microphone,
                        os().name
                    ),
                ),
            )
    }

    /// The microphones to choose from: Windows' default, then each plugged
    /// in, by name.
    fn microphone_choice(&self) -> SettingField<SharedString> {
        let mut choices = vec![(
            SharedString::default(),
            SharedString::from(os().default_microphone),
        )];
        choices.extend(
            self.microphones
                .iter()
                .map(|m| (SharedString::from(&m.id), SharedString::from(&m.name))),
        );
        let (read, write) = (self.hooks.clone(), self.hooks.clone());
        SettingField::dropdown(
            choices,
            move |_| {
                let chosen = read.settings.borrow().microphone.clone();
                chosen.unwrap_or_default().into()
            },
            move |id: SharedString, cx| {
                write.settings.borrow_mut().microphone = (!id.is_empty()).then(|| id.to_string());
                (write.changed)(cx);
            },
        )
    }

    fn recording(&self) -> SettingPage {
        let undo_chord = self.hooks.settings.borrow().undo_hotkey.clone();
        let seconds = |s: u32| format!("{s} seconds");
        SettingPage::new("Recording")
            .group(
                SettingGroup::new()
                    .item(heading("Capture", None))
                    .item(
                        SettingItem::new(
                            "Include the pointer",
                            self.switch(|s| s.record_cursor, |s, v| s.record_cursor = v),
                        )
                        .description("Screenshots have their own setting."),
                    )
                    .item(SettingItem::new(
                        "Frame rate",
                        self.dropdown(
                            vec![(30, "30 fps".into()), (60, "60 fps".into())],
                            Settings::record_fps,
                            |s, v| s.record_fps = v,
                        ),
                    ))
                    .item(
                        SettingItem::new(
                            "Count down first",
                            self.dropdown(
                                crate::settings::COUNTDOWN_CHOICES
                                    .map(|s| {
                                        let label = if s == 0 { "Off".into() } else { seconds(s) };
                                        (s, label)
                                    })
                                    .to_vec(),
                                Settings::countdown,
                                |s, v| s.recording_countdown = v,
                            ),
                        )
                        .description("Over the chosen area, never recorded. Esc cancels, Enter starts at once."),
                    ),
            )
            .group(self.sound())
            .group(self.recording_files())
            .group(
                SettingGroup::new()
                    .item(heading(
                        "While recording",
                        Some("These shortcuts are taken only while a recording runs (Undo only while there is something to undo); other apps have them the rest of the time."),
                    ))
                    .item(SettingItem::new("Pause and resume", self.hotkey(HotkeyKind::Pause)))
                    .item(SettingItem::new("Restart", self.hotkey(HotkeyKind::Restart)))
                    .item(SettingItem::new("Discard", self.hotkey(HotkeyKind::Discard)))
                    .item(SettingItem::new("Undo", self.hotkey(HotkeyKind::Undo))),
            )
            .group(
                SettingGroup::new()
                    .item(heading("Throwing a take away", None))
                    .item(
                        SettingItem::new(
                            "Ask before discarding or restarting",
                            self.switch(|s| s.confirm_discard, |s, v| s.confirm_discard = v),
                        )
                        .description(format!(
                            "Off: Discard and Restart act at once, and {undo_chord} brings the take back for a while."
                        )),
                    )
                    .item(
                        SettingItem::new(
                            "Undo lasts",
                            self.dropdown(
                                crate::settings::UNDO_CHOICES
                                    .map(|s| (s, seconds(s)))
                                    .to_vec(),
                                |s| s.undo_window().as_secs() as u32,
                                |s, v| s.undo_seconds = v,
                            ),
                        )
                        .description("When not asking first."),
                    ),
            )
    }

    fn diagnostics(&self) -> SettingPage {
        let d = &self.hooks.diagnostics;
        let info = |title: &str, value: String| {
            SettingItem::new(
                title.to_string(),
                SettingField::render(move |_, _, _| div().text_sm().child(value.clone())),
            )
        };
        let open = |title: &str, id: &'static str, path: Option<PathBuf>| {
            SettingItem::new(
                title.to_string(),
                SettingField::render(move |_, _, _| {
                    let path = path.clone();
                    Button::new(id)
                        .label("Open")
                        .disabled(path.is_none())
                        .on_click(move |_, _, cx| {
                            if let Some(path) = &path {
                                cx.open_with_system(path);
                            }
                        })
                }),
            )
            .description(
                d.log_dir
                    .as_ref()
                    .filter(|_| id == "open-logs")
                    .or(d.settings_path.as_ref().filter(|_| id == "open-settings"))
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            )
        };
        let displays = d.monitors.iter().map(|m| {
            let mode = if m.hdr_enabled {
                "HDR on"
            } else if m.advanced_color_enabled {
                "Advanced Color (SDR)"
            } else {
                "SDR"
            };
            let white = m
                .sdr_white_level_nits
                .map(|n| format!(" · SDR content at {n:.0} nits"))
                .unwrap_or_default();
            // Windows' number for it (Settings → Display), since two
            // monitors often share a model name.
            let number = m
                .device_name
                .rsplit("DISPLAY")
                .next()
                .filter(|n| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty());
            let name = match (m.name.is_empty(), number) {
                (false, Some(n)) => format!("{} (display {n})", m.name),
                (false, None) => m.name.clone(),
                (true, Some(n)) => format!("Display {n}"),
                (true, None) => m.device_name.clone(),
            };
            info(&name, mode.to_string()).description(format!(
                "{} × {} at {:.0}%{white} · {}",
                m.bounds.width,
                m.bounds.height,
                m.scale_factor * 100.0,
                m.adapter
            ))
        });
        SettingPage::new("Diagnostics")
            .group(
                SettingGroup::new()
                    .item(heading("Shuttercrab", None))
                    .item(info("Version", d.version.clone()))
                    .item(info(d.os.version_label, d.os.version.clone()))
                    .item(open("Log folder", "open-logs", d.log_dir.clone()))
                    .item(open(
                        "Settings file",
                        "open-settings",
                        d.settings_path.clone(),
                    )),
            )
            .group(
                SettingGroup::new()
                    .item(heading("Displays", None))
                    .items(displays),
            )
    }
}

/// The Fluent Emoji licence, which asks to travel with the art.
const FLUENT_EMOJI_LICENSE: &str = include_str!("../assets/emoji/LICENSE");

/// The Notices page: what Shuttercrab includes of others' work, with the
/// licences that ask to be shown.
fn notices() -> SettingPage {
    // The licence file is indented; shown as plain paragraphs.
    let license = FLUENT_EMOJI_LICENSE
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n");
    SettingPage::new("Notices").group(
        SettingGroup::new()
            .item(heading(
                "Third-party notices",
                Some("Work by others that Shuttercrab includes."),
            ))
            .item(
                SettingItem::new(
                    "Fluent Emoji",
                    SettingField::render(|_, _, _| div().text_sm().child("MIT licence")),
                )
                .description("The emoji, by Microsoft Corporation."),
            )
            .item(SettingItem::render(move |_, _, cx| {
                div()
                    .id("fluent-emoji-license")
                    .test_support()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(license.clone())
            })),
    )
}

/// A section heading inside a page. Sections are untitled groups, so the
/// sidebar lists only the pages (the owner found per-section entries
/// that merely scroll the page confusing).
fn heading(title: &'static str, description: Option<&'static str>) -> SettingItem {
    SettingItem::render(move |_, _, cx| {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .pt_2()
            .child(div().text_color(cx.theme().muted_foreground).child(title))
            .when_some(description, |d, text| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(text),
                )
            })
    })
}

/// Which output folder a row shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Folder {
    Screenshots,
    Recordings,
}

impl Folder {
    fn get(self, s: &Settings) -> PathBuf {
        match self {
            Self::Screenshots => s.output_dir(),
            Self::Recordings => s.recording_dir(),
        }
    }

    fn set(self, s: &mut Settings, dir: PathBuf) {
        match self {
            Self::Screenshots => s.output_dir = Some(dir),
            Self::Recordings => s.recording_dir = Some(dir),
        }
    }

    /// The ids of the row's text and buttons.
    fn ids(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Screenshots => ("output-folder", "choose-folder", "open-folder"),
            Self::Recordings => (
                "recording-folder",
                "choose-recording-folder",
                "open-recording-folder",
            ),
        }
    }

    fn prompt(self) -> &'static str {
        match self {
            Self::Screenshots => "Save screenshots here",
            Self::Recordings => "Save recordings here",
        }
    }
}

/// An output folder, with buttons to change and open it.
fn folder_row(hooks: Rc<Hooks>, folder: Folder) -> impl IntoElement {
    let dir = folder.get(&hooks.settings.borrow());
    let (pick, open) = (hooks.clone(), dir.clone());
    let (text_id, choose_id, open_id) = folder.ids();
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .id(text_id)
                .max_w(px(280.))
                .overflow_hidden()
                .text_sm()
                .child(dir.display().to_string()),
        )
        .child(
            Button::new(choose_id)
                .label("Change…")
                .on_click(move |_, _, cx| {
                    let chosen = cx.prompt_for_paths(PathPromptOptions {
                        files: false,
                        directories: true,
                        multiple: false,
                        prompt: Some(folder.prompt().into()),
                    });
                    let hooks = pick.clone();
                    cx.spawn(async move |cx| {
                        if let Ok(Ok(Some(mut paths))) = chosen.await
                            && let Some(dir) = paths.pop()
                        {
                            folder.set(&mut hooks.settings.borrow_mut(), dir);
                            cx.update(|cx| (hooks.changed)(cx));
                        }
                    })
                    .detach();
                }),
        )
        .child(
            Button::new(open_id)
                .label("Open")
                .on_click(move |_, _, cx| {
                    if std::fs::create_dir_all(&open).is_ok() {
                        cx.open_with_system(&open);
                    }
                }),
        )
}

impl Render for SettingsWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings-window")
            .role(Role::Pane)
            .aria_label("Shuttercrab Settings")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let recording = this.hotkeys.iter().any(|f| f.read(cx).is_recording());
                if event.keystroke.key == "escape" && !recording {
                    match this.on_escape.clone() {
                        Some(back) => back(window, cx),
                        None => crate::popup::close_window(window, cx),
                    }
                }
            }))
            .child(
                SettingsUi::new("shuttercrab-settings")
                    .sidebar_width(px(200.))
                    .pages([
                        self.general(),
                        self.screenshot(),
                        self.recording(),
                        self.diagnostics(),
                        notices(),
                    ]),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_presses_become_hotkeys() {
        assert_eq!(
            hotkey_from_keys(true, true, false, false, "c").as_deref(),
            Ok("Ctrl+Alt+C")
        );
        assert_eq!(
            hotkey_from_keys(false, true, true, false, "f5").as_deref(),
            Ok("Alt+Shift+F5")
        );
        assert_eq!(
            hotkey_from_keys(true, false, false, true, "space").as_deref(),
            Ok("Ctrl+Win+Space")
        );
        // Shift alone would swallow typing.
        assert!(hotkey_from_keys(false, false, true, false, "s").is_err());
        // Keys the hotkey parser does not know.
        assert!(hotkey_from_keys(true, true, false, false, "tab").is_err());
    }
}
