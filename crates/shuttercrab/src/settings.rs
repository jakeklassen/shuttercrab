//! User settings, stored as JSON in `%APPDATA%\Shuttercrab\settings.json`.
//!
//! Every field has a default, so a file from an older version (or with
//! fields removed by hand) still loads. A file that cannot be parsed is kept
//! as `settings.json.bad` rather than overwritten.

use crate::{
    capture_choice::{CaptureMode, CaptureTarget},
    markup::{Brush, Ink, Rgb, SHAPE_COLORS, ShapeKind, ShapeStyle, Tool},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Opens the Capture Bar, e.g. `Ctrl+Alt+C`.
    pub capture_bar_hotkey: String,
    /// Starts an area screenshot directly, e.g. `Ctrl+Alt+S`; Shuttercrab's
    /// window hides first.
    pub screenshot_hotkey: String,
    /// The same, with Shuttercrab's window left in the picture.
    pub screenshot_with_window_hotkey: String,
    /// Starts an area recording directly, or stops the one running.
    pub record_hotkey: String,
    /// Pause, restart and discard the recording; registered only while
    /// recording.
    pub pause_hotkey: String,
    pub restart_hotkey: String,
    pub discard_hotkey: String,
    /// Undoes a discard or keeps the take a restart replaced; registered
    /// only while there is something to undo.
    pub undo_hotkey: String,
    /// Put screenshots on the clipboard.
    pub copy_to_clipboard: bool,
    /// Also save each screenshot as a PNG file.
    pub auto_save: bool,
    /// Where screenshots are saved; `None` means `Pictures\Shuttercrab`.
    pub output_dir: Option<PathBuf>,
    /// Area selections snap to nearby window edges.
    pub snap_to_windows: bool,
    /// Single keys work in the main window (N for New, P for the pen, and
    /// so on); off, only Ctrl shortcuts and Escape do, against accidental
    /// presses. Off by default for now.
    pub single_key_shortcuts: bool,
    /// Draw the pointer into screenshots (PRD §15: off by default).
    pub include_cursor: bool,
    /// Seconds to wait before a screenshot started from the main window;
    /// one of [`DELAY_CHOICES`].
    pub screenshot_delay: u32,
    /// What the Capture Bar and the main window offer first: the last mode
    /// and target used.
    pub last_mode: CaptureMode,
    pub last_target: CaptureTarget,
    /// Show each screenshot in Shuttercrab's window, as Snipping Tool does,
    /// whatever started it. The window comes forward with the keyboard.
    pub show_in_window: bool,
    /// Show a thumbnail in the corner after each screenshot.
    pub show_thumbnail: bool,
    /// How long the thumbnail stays without the pointer over it.
    pub thumbnail_seconds: u32,
    /// Also show a Windows notification after each screenshot.
    pub notify_after_capture: bool,
    /// Where recordings are saved; `None` means `Videos\Shuttercrab`.
    pub recording_dir: Option<PathBuf>,
    /// Record the pointer (PRD §15: on by default for recordings).
    pub record_cursor: bool,
    /// Ask before Discard or Restart throws a take away; when off, they act
    /// at once and can be undone for a few seconds.
    pub confirm_discard: bool,
    /// How long Discard and Restart can be undone when they do not ask.
    pub undo_seconds: u32,
    /// Seconds to count down before recording starts; 0 starts at once.
    pub recording_countdown: u32,
    /// Show a Windows notification when a recording is saved.
    pub notify_after_recording: bool,
    /// 30 or 60 (PRD §13.1).
    pub record_fps: u32,
    /// The pen's and the highlighter's last colours (`#RRGGBB`) and sizes;
    /// read through [`Settings::brush`].
    pub pen_color: String,
    pub pen_size: f32,
    pub highlighter_color: String,
    pub highlighter_size: f32,
    /// The Shapes tool's last choices: the shape, its outline's and its
    /// fill's colours (`#RRGGBB`, or empty for Transparent) and opacities
    /// (percent), and the outline's size; read through
    /// [`Settings::shape_style`].
    pub shape: ShapeKind,
    pub shape_outline_color: String,
    pub shape_outline_opacity: u8,
    pub shape_fill_color: String,
    pub shape_fill_opacity: u8,
    pub shape_size: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            capture_bar_hotkey: "Ctrl+Alt+C".into(),
            screenshot_hotkey: "Ctrl+Alt+S".into(),
            screenshot_with_window_hotkey: "Ctrl+Alt+Shift+S".into(),
            record_hotkey: "Ctrl+Alt+R".into(),
            pause_hotkey: "Ctrl+Alt+P".into(),
            restart_hotkey: "Ctrl+Alt+N".into(),
            discard_hotkey: "Ctrl+Alt+D".into(),
            undo_hotkey: "Ctrl+Alt+Z".into(),
            copy_to_clipboard: true,
            auto_save: true,
            output_dir: None,
            snap_to_windows: true,
            include_cursor: false,
            single_key_shortcuts: false,
            last_mode: CaptureMode::Screenshot,
            last_target: CaptureTarget::Area,
            show_in_window: true,
            show_thumbnail: true,
            thumbnail_seconds: 6,
            notify_after_capture: false,
            recording_dir: None,
            record_cursor: true,
            record_fps: 30,
            recording_countdown: 0,
            screenshot_delay: 0,
            notify_after_recording: true,
            confirm_discard: true,
            undo_seconds: 10,
            pen_color: Brush::PEN.color.to_hex_string(),
            pen_size: Brush::PEN.size,
            highlighter_color: Brush::HIGHLIGHTER.color.to_hex_string(),
            highlighter_size: Brush::HIGHLIGHTER.size,
            shape: ShapeStyle::DEFAULT.kind,
            shape_outline_color: ink_text(ShapeStyle::DEFAULT.outline),
            shape_outline_opacity: ShapeStyle::DEFAULT.outline.opacity,
            shape_fill_color: ink_text(ShapeStyle::DEFAULT.fill),
            shape_fill_opacity: ShapeStyle::DEFAULT.fill.opacity,
            shape_size: ShapeStyle::DEFAULT.size,
        }
    }
}

impl Settings {
    /// `tool`'s colour and size, as last chosen; its default where the file
    /// holds something it does not offer.
    pub fn brush(&self, tool: Tool) -> Brush {
        let (color, size) = match tool {
            Tool::Pen => (&self.pen_color, self.pen_size),
            Tool::Highlighter => (&self.highlighter_color, self.highlighter_size),
        };
        let default = tool.default_brush();
        let sizes = tool.sizes();
        Brush {
            color: Rgb::from_hex_string(color)
                .filter(|c| tool.colors().contains(c))
                .unwrap_or(default.color),
            size: if sizes.contains(&size) {
                size.round()
            } else {
                default.size
            },
        }
    }

    pub fn set_brush(&mut self, tool: Tool, brush: Brush) {
        let color = brush.color.to_hex_string();
        match tool {
            Tool::Pen => (self.pen_color, self.pen_size) = (color, brush.size),
            Tool::Highlighter => {
                (self.highlighter_color, self.highlighter_size) = (color, brush.size)
            }
        }
    }

    /// The Shapes tool's choices, as last made; the default for any the
    /// file holds that it does not offer.
    pub fn shape_style(&self) -> ShapeStyle {
        let default = ShapeStyle::DEFAULT;
        let ink = |text: &str, opacity: u8, default: Ink| {
            let color = if text.is_empty() {
                Some(None)
            } else {
                Rgb::from_hex_string(text).map(Some)
            };
            match color.filter(|c| SHAPE_COLORS.contains(c)) {
                Some(color) => Ink {
                    color,
                    opacity: opacity.clamp(1, 100),
                },
                None => default,
            }
        };
        ShapeStyle {
            // An emoji is placed, never drawn next.
            kind: match self.shape {
                ShapeKind::Emoji(_) => default.kind,
                kind => kind,
            },
            outline: ink(
                &self.shape_outline_color,
                self.shape_outline_opacity,
                default.outline,
            ),
            fill: ink(
                &self.shape_fill_color,
                self.shape_fill_opacity,
                default.fill,
            ),
            size: if ShapeStyle::SIZES.contains(&self.shape_size) {
                self.shape_size.round()
            } else {
                default.size
            },
        }
    }

    pub fn set_shape_style(&mut self, style: ShapeStyle) {
        self.shape = style.kind;
        self.shape_outline_color = ink_text(style.outline);
        self.shape_outline_opacity = style.outline.opacity;
        self.shape_fill_color = ink_text(style.fill);
        self.shape_fill_opacity = style.fill.opacity;
        self.shape_size = style.size;
    }

    /// The folder screenshots are saved to.
    pub fn output_dir(&self) -> PathBuf {
        self.output_dir.clone().unwrap_or_else(default_output_dir)
    }

    /// The folder recordings are saved to.
    pub fn recording_dir(&self) -> PathBuf {
        self.recording_dir
            .clone()
            .unwrap_or_else(default_recording_dir)
    }

    /// The recording frame rate: 60 if asked for, otherwise 30.
    pub fn record_fps(&self) -> u32 {
        if self.record_fps == 60 { 60 } else { 30 }
    }

    /// How long Discard and Restart can be undone: one of
    /// [`UNDO_CHOICES`] seconds, 10 if the file says otherwise.
    pub fn undo_window(&self) -> std::time::Duration {
        let seconds = if UNDO_CHOICES.contains(&self.undo_seconds) {
            self.undo_seconds
        } else {
            10
        };
        std::time::Duration::from_secs(seconds.into())
    }
}

/// The countdowns the settings offer, in seconds; 0 is none.
pub const COUNTDOWN_CHOICES: [u32; 3] = [0, 3, 5];

/// The delays a screenshot from the main window can wait, in seconds; 0 is
/// none.
pub const DELAY_CHOICES: [u32; 4] = [0, 3, 5, 10];

impl Settings {
    /// The delay before a screenshot from the main window: one of
    /// [`DELAY_CHOICES`], none if the file says otherwise.
    pub fn delay(&self) -> u32 {
        if DELAY_CHOICES.contains(&self.screenshot_delay) {
            self.screenshot_delay
        } else {
            0
        }
    }

    /// The countdown before recording: one of [`COUNTDOWN_CHOICES`], none
    /// if the file says otherwise.
    pub fn countdown(&self) -> u32 {
        if COUNTDOWN_CHOICES.contains(&self.recording_countdown) {
            self.recording_countdown
        } else {
            0
        }
    }
}

/// The undo windows the settings offer, in seconds.
pub const UNDO_CHOICES: [u32; 4] = [5, 10, 20, 30];

/// `Pictures\Shuttercrab`, or a `Shuttercrab` folder in the home directory when
/// there is no Pictures folder.
pub fn default_output_dir() -> PathBuf {
    dirs::picture_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("Shuttercrab")
}

/// `Videos\Shuttercrab`, or `Shuttercrab Videos` beside the screenshots folder
/// when there is no Videos folder.
pub fn default_recording_dir() -> PathBuf {
    match dirs::video_dir() {
        Some(videos) => videos.join("Shuttercrab"),
        None => default_output_dir().with_file_name("Shuttercrab Videos"),
    }
}

/// `%APPDATA%\Shuttercrab\settings.json`.
pub fn default_path() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("Shuttercrab")
            .join("settings.json"),
    )
}

/// What loading found, so the caller can log it.
#[derive(Debug, PartialEq, Eq)]
pub enum Loaded {
    Read,
    /// There was no file; defaults were written.
    Created,
    /// The file could not be parsed; it was moved aside and defaults used.
    Replaced(PathBuf),
}

/// Read the settings at `path`, creating the file with defaults if it does
/// not exist.
pub fn load(path: &Path) -> Result<(Settings, Loaded)> {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(settings) => Ok((settings, Loaded::Read)),
            Err(_) => {
                let bad = path.with_extension("json.bad");
                std::fs::rename(path, &bad)
                    .with_context(|| format!("moving aside {}", path.display()))?;
                let settings = Settings::default();
                save(path, &settings)?;
                Ok((settings, Loaded::Replaced(bad)))
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let settings = Settings::default();
            save(path, &settings)?;
            Ok((settings, Loaded::Created))
        }
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Write the settings, replacing the file only once the new one is complete.
pub fn save(path: &Path, settings: &Settings) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let partial = path.with_extension("json.tmp");
    std::fs::write(&partial, serde_json::to_string_pretty(settings)?)
        .with_context(|| format!("writing {}", partial.display()))?;
    std::fs::rename(&partial, path).with_context(|| format!("replacing {}", path.display()))
}

/// An ink's colour as settings keep it: `#RRGGBB`, or empty for
/// Transparent.
fn ink_text(ink: Ink) -> String {
    ink.color.map(Rgb::to_hex_string).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_choices_are_remembered_and_odd_values_fall_back() {
        let mut settings = Settings::default();
        assert_eq!(settings.shape_style(), ShapeStyle::DEFAULT);
        let chosen = ShapeStyle {
            kind: ShapeKind::Arrow,
            outline: Ink::TRANSPARENT,
            fill: Ink {
                color: Some(Rgb(0x00, 0x4D, 0xE6)),
                opacity: 40,
            },
            size: 9.,
        };
        settings.set_shape_style(chosen);
        assert_eq!(settings.shape_style(), chosen);
        // Iron gray is the pen's, not a shape's; sizes stop at 24.
        settings.shape_outline_color = "#58595B".into();
        settings.shape_size = 30.;
        let style = settings.shape_style();
        assert_eq!(style.outline, ShapeStyle::DEFAULT.outline);
        assert_eq!(style.size, ShapeStyle::DEFAULT.size);
        assert_eq!(style.fill, chosen.fill);
    }

    #[test]
    fn brushes_are_remembered_and_odd_values_fall_back() {
        let mut settings = Settings::default();
        assert_eq!(settings.brush(Tool::Pen), Brush::PEN);
        let blue = Brush {
            color: Rgb(0x00, 0x4D, 0xE6),
            size: 7.,
        };
        settings.set_brush(Tool::Pen, blue);
        assert_eq!(settings.brush(Tool::Pen), blue);
        assert_eq!(settings.brush(Tool::Highlighter), Brush::HIGHLIGHTER);
        // A colour the tool does not offer, or a size out of range.
        settings.highlighter_color = "#123456".into();
        settings.highlighter_size = 200.;
        assert_eq!(settings.brush(Tool::Highlighter), Brush::HIGHLIGHTER);
    }
    /// A folder of the test's own under the workspace's gitignored `tmp`,
    /// removed when the test ends, passed or not.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tmp/tests")
                .join(format!("settings-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }

        fn settings(&self) -> PathBuf {
            self.0.join("settings.json")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn creates_defaults_then_reads_them_back() {
        let scratch = Scratch::new("create");
        let path = scratch.settings();
        let (settings, loaded) = load(&path).unwrap();
        assert_eq!(
            (settings.clone(), loaded),
            (Settings::default(), Loaded::Created)
        );
        let changed = Settings {
            auto_save: false,
            screenshot_hotkey: "Ctrl+Shift+F1".into(),
            ..settings
        };
        save(&path, &changed).unwrap();
        assert_eq!(load(&path).unwrap(), (changed, Loaded::Read));
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let scratch = Scratch::new("partial");
        let path = scratch.settings();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{ "auto_save": false }"#).unwrap();
        let (settings, _) = load(&path).unwrap();
        assert!(!settings.auto_save);
        assert_eq!(settings.screenshot_hotkey, "Ctrl+Alt+S");
        assert!(settings.copy_to_clipboard);
    }

    #[test]
    fn a_broken_file_is_kept_aside_not_lost() {
        let scratch = Scratch::new("broken");
        let path = scratch.settings();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        let (settings, loaded) = load(&path).unwrap();
        assert_eq!(settings, Settings::default());
        let Loaded::Replaced(bad) = loaded else {
            panic!("expected the file to be replaced")
        };
        assert_eq!(std::fs::read_to_string(bad).unwrap(), "{ not json");
    }

    #[test]
    fn the_default_folder_is_under_pictures() {
        let dir = Settings::default().output_dir();
        assert!(dir.ends_with("Shuttercrab"), "{}", dir.display());
    }

    #[test]
    fn recordings_default_to_their_own_folder_with_the_pointer_at_30_fps() {
        let settings = Settings::default();
        assert_ne!(settings.recording_dir(), settings.output_dir());
        assert!(settings.record_cursor);
        assert_eq!(settings.record_fps(), 30);
        let odd = Settings {
            record_fps: 45,
            ..settings
        };
        assert_eq!(odd.record_fps(), 30);
    }

    #[test]
    fn recordings_start_at_once_and_notify_when_saved_by_default() {
        let settings = Settings::default();
        assert_eq!(settings.countdown(), 0);
        assert!(settings.notify_after_recording);
        let three = Settings {
            recording_countdown: 3,
            ..settings.clone()
        };
        assert_eq!(three.countdown(), 3);
        let odd = Settings {
            recording_countdown: 4,
            ..settings
        };
        assert_eq!(odd.countdown(), 0);
    }

    #[test]
    fn throwing_a_take_away_asks_first_and_undo_lasts_a_choice_of_seconds() {
        use std::time::Duration;
        let settings = Settings::default();
        assert!(settings.confirm_discard);
        assert_eq!(settings.undo_window(), Duration::from_secs(10));
        let longer = Settings {
            undo_seconds: 30,
            ..settings.clone()
        };
        assert_eq!(longer.undo_window(), Duration::from_secs(30));
        let odd = Settings {
            undo_seconds: 0,
            ..settings
        };
        assert_eq!(odd.undo_window(), Duration::from_secs(10));
    }
}
