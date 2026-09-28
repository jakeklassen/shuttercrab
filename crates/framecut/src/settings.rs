//! User settings, stored as JSON in `%APPDATA%\Framecut\settings.json`.
//!
//! Every field has a default, so a file from an older version (or with
//! fields removed by hand) still loads. A file that cannot be parsed is kept
//! as `settings.json.bad` rather than overwritten.

use crate::capture_bar::CaptureTarget;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Opens the Capture Bar, e.g. `Ctrl+Alt+C`.
    pub capture_bar_hotkey: String,
    /// Starts an area screenshot directly, e.g. `Ctrl+Alt+S`.
    pub screenshot_hotkey: String,
    /// Put screenshots on the clipboard.
    pub copy_to_clipboard: bool,
    /// Also save each screenshot as a PNG file.
    pub auto_save: bool,
    /// Where screenshots are saved; `None` means `Pictures\Framecut`.
    pub output_dir: Option<PathBuf>,
    /// Area selections snap to nearby window edges.
    pub snap_to_windows: bool,
    /// What the Capture Bar offers first: the last target used.
    pub last_target: CaptureTarget,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            capture_bar_hotkey: "Ctrl+Alt+C".into(),
            screenshot_hotkey: "Ctrl+Alt+S".into(),
            copy_to_clipboard: true,
            auto_save: true,
            output_dir: None,
            snap_to_windows: true,
            last_target: CaptureTarget::Area,
        }
    }
}

impl Settings {
    /// The folder screenshots are saved to.
    pub fn output_dir(&self) -> PathBuf {
        self.output_dir.clone().unwrap_or_else(default_output_dir)
    }
}

/// `Pictures\Framecut`, or a `Framecut` folder in the home directory when
/// there is no Pictures folder.
pub fn default_output_dir() -> PathBuf {
    dirs::picture_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("Framecut")
}

/// `%APPDATA%\Framecut\settings.json`.
pub fn default_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("Framecut").join("settings.json"))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("framecut-settings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("settings.json")
    }

    #[test]
    fn creates_defaults_then_reads_them_back() {
        let path = temp("create");
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
        let path = temp("partial");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{ "auto_save": false }"#).unwrap();
        let (settings, _) = load(&path).unwrap();
        assert!(!settings.auto_save);
        assert_eq!(settings.screenshot_hotkey, "Ctrl+Alt+S");
        assert!(settings.copy_to_clipboard);
    }

    #[test]
    fn a_broken_file_is_kept_aside_not_lost() {
        let path = temp("broken");
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
        assert!(dir.ends_with("Framecut"), "{}", dir.display());
    }
}
