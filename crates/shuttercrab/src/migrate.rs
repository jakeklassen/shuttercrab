//! Shuttercrab was called Framecut until 2026-10-02. The first time
//! Shuttercrab starts, it carries Framecut's settings over: the hotkeys and
//! choices come across unchanged, and folders that were Framecut's defaults
//! become Shuttercrab's. Captures already saved stay where they are, and
//! Framecut's own files are left in place.

use crate::settings::{self, Settings};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The name settings, folders and the startup entry used before the rename.
pub const OLD_NAME: &str = "Framecut";

/// `%APPDATA%\Framecut\settings.json`.
pub fn old_settings_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join(OLD_NAME).join("settings.json"))
}

/// The folders Framecut saved into by default: `Pictures\Framecut`,
/// `Videos\Framecut`, and their fallbacks.
pub fn old_default_dirs() -> Vec<PathBuf> {
    let pictures = dirs::picture_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join(OLD_NAME);
    let mut dirs = vec![pictures.with_file_name(format!("{OLD_NAME} Videos"))];
    if let Some(videos) = dirs::video_dir() {
        dirs.push(videos.join(OLD_NAME));
    }
    dirs.push(pictures);
    dirs
}

/// Copy the settings at `old` to `new`, unless `new` already exists or `old`
/// does not. A folder setting that is one of `old_defaults` is cleared, so
/// new captures go to Shuttercrab's default folders. Returns whether
/// settings were carried over.
pub fn carry_settings(old: &Path, new: &Path, old_defaults: &[PathBuf]) -> Result<bool> {
    if new.exists() {
        return Ok(false);
    }
    let text = match std::fs::read_to_string(old) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).with_context(|| format!("reading {}", old.display())),
    };
    let mut carried: Settings =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", old.display()))?;
    for dir in [&mut carried.output_dir, &mut carried.recording_dir] {
        if dir
            .as_deref()
            .is_some_and(|d| old_defaults.iter().any(|o| same_dir(d, o)))
        {
            *dir = None;
        }
    }
    settings::save(new, &carried)?;
    Ok(true)
}

/// Windows paths compare without regard to case or a trailing separator.
fn same_dir(a: &Path, b: &Path) -> bool {
    let normal = |p: &Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    normal(a) == normal(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of the test's own under the workspace's gitignored `tmp`,
    /// removed when the test ends, passed or not.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tmp/tests")
                .join(format!("migrate-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn settings_come_across_and_default_folders_follow_the_new_name() {
        let scratch = Scratch::new("carry");
        let old = scratch.0.join("Framecut/settings.json");
        let new = scratch.0.join("Shuttercrab/settings.json");
        let pictures = scratch.0.join("Pictures/Framecut");
        let custom = scratch.0.join("Somewhere/Else");
        let framecut = Settings {
            screenshot_hotkey: "Ctrl+Shift+F1".into(),
            output_dir: Some(scratch.0.join("pictures/framecut/")),
            recording_dir: Some(custom.clone()),
            ..Settings::default()
        };
        settings::save(&old, &framecut).unwrap();

        assert!(carry_settings(&old, &new, &[pictures]).unwrap());
        let (carried, _) = settings::load(&new).unwrap();
        assert_eq!(carried.screenshot_hotkey, "Ctrl+Shift+F1");
        // The old default, spelled differently, now follows the new default…
        assert_eq!(carried.output_dir, None);
        // …and a folder the user chose stays.
        assert_eq!(carried.recording_dir, Some(custom));
        // Framecut's file is left alone.
        assert!(old.exists());
    }

    #[test]
    fn nothing_is_carried_over_twice_or_from_nowhere() {
        let scratch = Scratch::new("once");
        let old = scratch.0.join("Framecut/settings.json");
        let new = scratch.0.join("Shuttercrab/settings.json");
        assert!(!carry_settings(&old, &new, &[]).unwrap());
        assert!(!new.exists());

        settings::save(&old, &Settings::default()).unwrap();
        let mine = Settings {
            auto_save: false,
            ..Settings::default()
        };
        settings::save(&new, &mine).unwrap();
        assert!(!carry_settings(&old, &new, &[]).unwrap());
        assert_eq!(settings::load(&new).unwrap().0, mine);
    }
}
