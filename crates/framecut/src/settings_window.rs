//! The settings window (PRD §26): General, Screenshot and Diagnostics
//! pages. Every change applies at once; there is no Save button. The whole
//! window works from the keyboard: Tab between controls, Space or Enter to
//! use them, Escape to close.
//!
//! The window reaches the rest of Framecut only through [`Hooks`], so it
//! can be tested on its own.

use crate::settings::Settings;
use framecut_capture::MonitorInfo;
use framecut_platform::Hotkey;
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
use std::{cell::RefCell, path::PathBuf, rc::Rc};

/// What the settings window needs from the rest of Framecut.
pub struct Hooks {
    /// The live settings, shared with the capture flows.
    pub settings: Rc<RefCell<Settings>>,
    /// After any change: save the settings and refresh the tray menu.
    pub changed: Rc<dyn Fn(&mut App)>,
    /// Unregister the global hotkeys, while a new one is being recorded.
    pub pause_hotkeys: Rc<dyn Fn()>,
    /// Register the hotkeys the settings name. Resolves to the ids of those
    /// another application already owns.
    pub apply_hotkeys: Rc<dyn Fn() -> LocalBoxFuture<'static, Vec<u32>>>,
    /// Whether Framecut starts at sign-in, and a way to change it.
    pub launch_at_startup: Rc<dyn Fn() -> bool>,
    pub set_launch_at_startup: Rc<dyn Fn(bool)>,
    /// What the Diagnostics page shows.
    pub diagnostics: Diagnostics,
}

/// Facts about this machine and this copy of Framecut.
#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    pub version: String,
    pub windows_build: u32,
    pub monitors: Vec<MonitorInfo>,
    pub log_dir: Option<PathBuf>,
    pub settings_path: Option<PathBuf>,
}

/// Which hotkey a recorder edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyKind {
    CaptureBar,
    Screenshot,
}

impl HotkeyKind {
    fn get(self, s: &Settings) -> &str {
        match self {
            HotkeyKind::CaptureBar => &s.capture_bar_hotkey,
            HotkeyKind::Screenshot => &s.screenshot_hotkey,
        }
    }

    fn set(self, s: &mut Settings, value: String) {
        match self {
            HotkeyKind::CaptureBar => s.capture_bar_hotkey = value,
            HotkeyKind::Screenshot => s.screenshot_hotkey = value,
        }
    }

    fn other(self) -> HotkeyKind {
        match self {
            HotkeyKind::CaptureBar => HotkeyKind::Screenshot,
            HotkeyKind::Screenshot => HotkeyKind::CaptureBar,
        }
    }

    fn label(self) -> &'static str {
        match self {
            HotkeyKind::CaptureBar => "the Capture Bar",
            HotkeyKind::Screenshot => "area screenshots",
        }
    }
}

/// The hotkey a key press makes, as Framecut writes it, or why it cannot
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
            "Include Ctrl, Alt or Win, so the shortcut does not get in the way of typing.".into(),
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
    /// The id the app registers this hotkey under.
    id: u32,
    hooks: Rc<Hooks>,
    recording: bool,
    message: Option<SharedString>,
    focus: FocusHandle,
}

impl HotkeyField {
    pub fn new(
        kind: HotkeyKind,
        id: u32,
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
        Self {
            kind,
            id,
            hooks,
            recording: false,
            message: None,
            focus,
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recording
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.recording = true;
        self.message = None;
        // Otherwise pressing the current hotkey would start a capture.
        (self.hooks.pause_hotkeys)();
        cx.notify();
    }

    /// Stop recording and register the hotkeys the settings name.
    fn stop(&mut self, cx: &mut Context<Self>) {
        self.recording = false;
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
        let hotkey = match hotkey_from_keys(m.control, m.alt, m.shift, m.platform, &keystroke.key) {
            Ok(hotkey) => hotkey,
            Err(message) => {
                self.message = Some(message.into());
                cx.notify();
                return;
            }
        };
        let other = self.kind.other();
        if hotkey == other.get(&self.hooks.settings.borrow()) {
            self.message = Some(format!("{hotkey} already opens {}.", other.label()).into());
            cx.notify();
            return;
        }
        let previous = self.kind.get(&self.hooks.settings.borrow()).to_string();
        self.kind
            .set(&mut self.hooks.settings.borrow_mut(), hotkey.clone());
        self.recording = false;
        cx.notify();
        let (applied, id, kind) = ((self.hooks.apply_hotkeys)(), self.id, self.kind);
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
        let id = match self.kind {
            HotkeyKind::CaptureBar => "hotkey-capture-bar",
            HotkeyKind::Screenshot => "hotkey-screenshot",
        };
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

/// The settings window's content.
pub struct SettingsWindow {
    hooks: Rc<Hooks>,
    capture_bar: Entity<HotkeyField>,
    screenshot: Entity<HotkeyField>,
    focus: FocusHandle,
}

impl SettingsWindow {
    pub fn new(
        hooks: Rc<Hooks>,
        hotkey_ids: (u32, u32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let capture_bar = cx.new(|cx| {
            HotkeyField::new(
                HotkeyKind::CaptureBar,
                hotkey_ids.0,
                hooks.clone(),
                window,
                cx,
            )
        });
        let screenshot = cx.new(|cx| {
            HotkeyField::new(
                HotkeyKind::Screenshot,
                hotkey_ids.1,
                hooks.clone(),
                window,
                cx,
            )
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            hooks,
            capture_bar,
            screenshot,
            focus,
        }
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
        let (capture_bar, screenshot) = (self.capture_bar.clone(), self.screenshot.clone());
        let folder = hooks.clone();
        SettingPage::new("General")
            .default_open(true)
            .group(
                SettingGroup::new().item(heading("Startup", None)).item(
                    SettingItem::new(
                        "Start Framecut when you sign in",
                        SettingField::switch(
                            move |_| (startup_get.launch_at_startup)(),
                            move |value, _| (startup_set.set_launch_at_startup)(value),
                        ),
                    )
                    .description("Framecut waits quietly in the tray until you capture."),
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
                        SettingField::render(move |_, _, _| capture_bar.clone()),
                    ))
                    .item(SettingItem::new(
                        "Screenshot an area",
                        SettingField::render(move |_, _, _| screenshot.clone()),
                    )),
            )
            .group(
                SettingGroup::new()
                    .item(heading("Where screenshots go", None))
                    .item(SettingItem::new(
                        "Copy to the clipboard",
                        self.switch(|s| s.copy_to_clipboard, |s, v| s.copy_to_clipboard = v),
                    ))
                    .item(SettingItem::new(
                        "Save to the folder",
                        self.switch(|s| s.auto_save, |s, v| s.auto_save = v),
                    ))
                    .item(SettingItem::new(
                        "Folder",
                        SettingField::render(move |_, _, _| folder_row(folder.clone())),
                    )),
            )
    }

    fn screenshot(&self) -> SettingPage {
        let (read, write) = (self.hooks.clone(), self.hooks.clone());
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
                    .item(heading("After a capture", None))
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
                        .description("A Windows notification with the file name."),
                    ),
            )
    }

    fn recording(&self) -> SettingPage {
        let (read, write) = (self.hooks.clone(), self.hooks.clone());
        let undo_choices = crate::settings::UNDO_CHOICES
            .map(|s| {
                (
                    SharedString::from(s.to_string()),
                    SharedString::from(format!("{s} seconds")),
                )
            })
            .to_vec();
        SettingPage::new("Recording").group(
            SettingGroup::new()
                .item(heading("Throwing a take away", None))
                .item(
                    SettingItem::new(
                        "Ask before discarding or restarting",
                        self.switch(|s| s.confirm_discard, |s, v| s.confirm_discard = v),
                    )
                    .description(
                        "Off: Discard and Restart act at once, and the undo chord (Ctrl+Alt+Z) \
                         brings the take back for a while.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Undo lasts",
                        SettingField::dropdown(
                            undo_choices,
                            move |_| {
                                let seconds = read.settings.borrow().undo_window().as_secs();
                                seconds.to_string().into()
                            },
                            move |value, cx| {
                                if let Ok(seconds) = value.parse() {
                                    write.settings.borrow_mut().undo_seconds = seconds;
                                    (write.changed)(cx);
                                }
                            },
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
                    .item(heading("Framecut", None))
                    .item(info("Version", d.version.clone()))
                    .item(info("Windows build", d.windows_build.to_string()))
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

/// The output folder, with buttons to change and open it.
fn folder_row(hooks: Rc<Hooks>) -> impl IntoElement {
    let dir = hooks.settings.borrow().output_dir();
    let (pick, open) = (hooks.clone(), dir.clone());
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .id("output-folder")
                .max_w(px(280.))
                .overflow_hidden()
                .text_sm()
                .child(dir.display().to_string()),
        )
        .child(
            Button::new("choose-folder")
                .label("Change…")
                .on_click(move |_, _, cx| {
                    let chosen = cx.prompt_for_paths(PathPromptOptions {
                        files: false,
                        directories: true,
                        multiple: false,
                        prompt: Some("Save screenshots here".into()),
                    });
                    let hooks = pick.clone();
                    cx.spawn(async move |cx| {
                        if let Ok(Ok(Some(mut paths))) = chosen.await
                            && let Some(dir) = paths.pop()
                        {
                            hooks.settings.borrow_mut().output_dir = Some(dir);
                            cx.update(|cx| (hooks.changed)(cx));
                        }
                    })
                    .detach();
                }),
        )
        .child(
            Button::new("open-folder")
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
            .aria_label("Framecut Settings")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let recording = this.capture_bar.read(cx).is_recording()
                    || this.screenshot.read(cx).is_recording();
                if event.keystroke.key == "escape" && !recording {
                    crate::popup::close_window(window, cx);
                }
            }))
            .child(
                SettingsUi::new("framecut-settings")
                    .sidebar_width(px(200.))
                    .pages([
                        self.general(),
                        self.screenshot(),
                        self.recording(),
                        self.diagnostics(),
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
