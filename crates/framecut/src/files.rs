//! Output files: `Capture 2026-09-22 13-42-18.png` and
//! `Recording 2026-09-22 13-45-03.mp4` (PRD §12).

use anyhow::{Context, Result};
use chrono::NaiveDateTime;
use std::path::{Path, PathBuf};

/// The file name for a screenshot taken at `at` (local time).
pub fn capture_name(at: NaiveDateTime) -> String {
    format!("Capture {}.png", at.format("%Y-%m-%d %H-%M-%S"))
}

/// The file name for a recording started at `at` (local time).
pub fn recording_name(at: NaiveDateTime) -> String {
    format!("Recording {}.mp4", at.format("%Y-%m-%d %H-%M-%S"))
}

/// The extension a recording has until it is finished.
const PARTIAL: &str = "partial";

/// A path in `dir` for `name` that does not exist yet: `name`, then
/// `name (2)`, `name (3)`… for screenshots taken within the same second.
pub fn unused_path(dir: &Path, name: &str) -> PathBuf {
    let free = |p: &Path| !p.exists() && !partial_path(p).exists();
    let first = dir.join(name);
    if free(&first) {
        return first;
    }
    let (stem, extension) = name.rsplit_once('.').unwrap_or((name, ""));
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}).{extension}")))
        .find(|p| free(p))
        .expect("some suffix is free")
}

/// Where a recording is written until it is finished: `name.mp4.partial`,
/// so an interrupted recording is never mistaken for a finished one.
pub fn partial_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(PARTIAL);
    PathBuf::from(name)
}

/// The path for a new recording in `dir`, creating the folder if needed.
pub fn new_recording(dir: &Path, at: NaiveDateTime) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(unused_path(dir, &recording_name(at)))
}

/// Save `png` as a new screenshot in `dir`, creating the folder if needed.
/// Returns the path written.
pub fn save_screenshot(dir: &Path, at: NaiveDateTime, png: &[u8]) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = unused_path(dir, &capture_name(at));
    std::fs::write(&path, png).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Where screenshots that were not auto-saved go when the thumbnail needs a
/// file to open or drag: `%TEMP%\Framecut`.
pub fn temp_dir() -> PathBuf {
    std::env::temp_dir().join("Framecut")
}

/// Delete PNG files in `dir` last written more than `age` ago. Returns how
/// many were deleted.
pub fn remove_old(dir: &Path, age: std::time::Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
        .filter(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|elapsed| elapsed > age)
        })
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder for the test under the workspace's gitignored `tmp`.
    fn scratch(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tmp/tests")
            .join(format!("{name}-{}", std::process::id()))
    }
    use chrono::NaiveDate;

    fn at() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 22)
            .unwrap()
            .and_hms_opt(13, 42, 18)
            .unwrap()
    }

    #[test]
    fn names_follow_the_prd() {
        assert_eq!(capture_name(at()), "Capture 2026-09-22 13-42-18.png");
        assert_eq!(recording_name(at()), "Recording 2026-09-22 13-42-18.mp4");
    }

    #[test]
    fn a_recording_in_progress_keeps_its_name_taken() {
        let dir = scratch("rec");
        let _ = std::fs::remove_dir_all(&dir);
        let first = new_recording(&dir, at()).unwrap();
        assert_eq!(
            partial_path(&first).file_name().unwrap(),
            "Recording 2026-09-22 13-42-18.mp4.partial"
        );
        std::fs::write(partial_path(&first), b"").unwrap();
        let second = new_recording(&dir, at()).unwrap();
        assert_eq!(
            second.file_name().unwrap(),
            "Recording 2026-09-22 13-42-18 (2).mp4"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn screenshots_in_the_same_second_get_numbered() {
        let dir = scratch("files");
        let _ = std::fs::remove_dir_all(&dir);
        let first = save_screenshot(&dir, at(), b"one").unwrap();
        let second = save_screenshot(&dir, at(), b"two").unwrap();
        let third = save_screenshot(&dir, at(), b"three").unwrap();
        assert_eq!(
            first.file_name().unwrap(),
            "Capture 2026-09-22 13-42-18.png"
        );
        assert_eq!(
            second.file_name().unwrap(),
            "Capture 2026-09-22 13-42-18 (2).png"
        );
        assert_eq!(
            third.file_name().unwrap(),
            "Capture 2026-09-22 13-42-18 (3).png"
        );
        assert_eq!(std::fs::read(second).unwrap(), b"two");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn old_files_are_removed_and_new_ones_kept() {
        let dir = scratch("old");
        let _ = std::fs::remove_dir_all(&dir);
        let png = save_screenshot(&dir, at(), b"png").unwrap();
        std::fs::write(dir.join("notes.txt"), b"keep").unwrap();
        assert_eq!(remove_old(&dir, std::time::Duration::from_secs(3600)), 0);
        assert!(png.exists());
        assert_eq!(remove_old(&dir, std::time::Duration::ZERO), 1);
        assert!(!png.exists() && dir.join("notes.txt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
