//! Shuttercrab's launcher entry on Linux: a freedesktop `.desktop` file and
//! its icon in the user's data folder, as apps no package manager installs
//! add for themselves. GNOME takes a window's dock icon, and the name and
//! icon of its notifications, from the entry whose `StartupWMClass` is the
//! window's class. Written at each start, so it follows the program if it
//! moves; a file already up to date is left alone.

use crate::{Request, icon, launcher::APP_ID};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The icon's side, pixels.
const ICON_SIZE: u32 = 256;

/// Add or update the entry and its icon.
pub fn register() -> Result<()> {
    let data = data_dir().context("no home folder")?;
    let program = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .context("no path to the program")?;
    let icon = icon_path_in(&data);
    write_if_changed(&icon, &icon::png(ICON_SIZE))?;
    let entry = data.join("applications").join(format!("{APP_ID}.desktop"));
    write_if_changed(&entry, desktop_entry(&program, &icon).as_bytes())
}

/// Where [`register`] puts the icon, for notifications to show it.
pub(crate) fn icon_path() -> Option<PathBuf> {
    data_dir().map(|data| icon_path_in(&data))
}

/// `$XDG_DATA_HOME`, or `~/.local/share`.
fn data_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::home_dir().map(|home| home.join(".local/share")))
}

fn icon_path_in(data: &Path) -> PathBuf {
    data.join(format!(
        "icons/hicolor/{ICON_SIZE}x{ICON_SIZE}/apps/{APP_ID}.png"
    ))
}

fn write_if_changed(path: &Path, contents: &[u8]) -> Result<()> {
    if std::fs::read(path).is_ok_and(|now| now == contents) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

/// The entry: Shuttercrab, with its capture requests as actions (a dock
/// icon's menu offers them).
fn desktop_entry(program: &Path, icon: &Path) -> String {
    let exec = exec_arg(&program.to_string_lossy());
    let mut entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Shuttercrab\n\
         Comment=Take screenshots and record the screen\n\
         Exec={exec}\n\
         Icon={}\n\
         Terminal=false\n\
         Categories=Graphics;Utility;\n\
         Keywords=screenshot;screen recording;capture;snip;\n\
         StartupWMClass={APP_ID}\n\
         Actions=capture-bar;screenshot;record;\n",
        icon.display()
    );
    for (request, name) in [
        (Request::CaptureBar, "Open the Capture Bar"),
        (Request::Screenshot, "Take a screenshot"),
        (Request::Record, "Record the screen"),
    ] {
        entry.push_str(&format!(
            "\n[Desktop Action {0}]\nName={name}\nExec={exec} --{0}\n",
            request.name()
        ));
    }
    entry
}

/// `arg` quoted for an `Exec` key: in double quotes, with the characters
/// the spec reserves inside them escaped, then escaped again as a string
/// value; a literal `%` is doubled.
fn exec_arg(arg: &str) -> String {
    let mut quoted = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' => quoted.push_str("\\\\"),
            '\\' => quoted.push_str("\\\\\\"),
            '%' => quoted.push('%'),
            _ => {}
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_the_program_for_exec() {
        assert_eq!(
            exec_arg("/home/me/Apps/shuttercrab"),
            "\"/home/me/Apps/shuttercrab\""
        );
        assert_eq!(exec_arg("/home/me/My Apps/s"), "\"/home/me/My Apps/s\"");
        assert_eq!(exec_arg("/a$b"), "\"/a\\\\$b\"");
        assert_eq!(exec_arg("/a\\b"), "\"/a\\\\\\\\b\"");
        assert_eq!(exec_arg("/100%"), "\"/100%%\"");
    }

    #[test]
    fn the_entry_names_the_window_class_and_offers_the_captures() {
        let entry = desktop_entry(
            Path::new("/opt/Shuttercrab/shuttercrab"),
            Path::new("/home/me/.local/share/icons/hicolor/256x256/apps/shuttercrab.png"),
        );
        assert!(entry.starts_with("[Desktop Entry]\nType=Application\nName=Shuttercrab\n"));
        assert!(entry.contains("\nExec=\"/opt/Shuttercrab/shuttercrab\"\n"));
        assert!(entry.contains("\nStartupWMClass=shuttercrab\n"));
        assert!(entry.contains(
            "\n[Desktop Action screenshot]\nName=Take a screenshot\nExec=\"/opt/Shuttercrab/shuttercrab\" --screenshot\n"
        ));
    }
}
