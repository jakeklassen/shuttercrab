//! Launching Shuttercrab when the user signs in (PRD §26): an XDG autostart
//! entry, `~/.config/autostart/shuttercrab.desktop`, which GNOME's Startup
//! Applications and other desktops' settings also show and switch.

use super::launcher::{exec_arg, icon_path, program, write_if_changed};
use crate::{launcher::APP_ID, startup::BACKGROUND_FLAG};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Whether Shuttercrab starts when the user signs in.
pub fn launch_at_startup() -> bool {
    entry_path().is_some_and(|path| path.exists())
}

/// Start (or stop starting) this program when the user signs in, in the
/// tray.
pub fn set_launch_at_startup(enabled: bool) -> Result<()> {
    let path = entry_path().context("no home folder")?;
    if enabled {
        let entry = autostart_entry(&program()?, icon_path().as_deref());
        write_if_changed(&path, entry.as_bytes())
    } else {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("removing {}", path.display()))
            }
            _ => Ok(()),
        }
    }
}

/// Every entry Shuttercrab wrote here starts it in the tray.
pub fn add_background_flag() -> Result<bool> {
    Ok(false)
}

/// `$XDG_CONFIG_HOME/autostart`, or `~/.config/autostart`, and the entry's
/// name there.
fn entry_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::home_dir().map(|home| home.join(".config")))?;
    Some(config.join("autostart").join(format!("{APP_ID}.desktop")))
}

fn autostart_entry(program: &Path, icon: Option<&Path>) -> String {
    let exec = exec_arg(&program.to_string_lossy());
    let icon = icon.map_or(String::new(), |icon| format!("Icon={}\n", icon.display()));
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Shuttercrab\n\
         Comment=Take screenshots and record the screen\n\
         Exec={exec} {BACKGROUND_FLAG}\n\
         {icon}\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_starts_the_program_in_the_tray() {
        let entry = autostart_entry(
            Path::new("/home/me/My Apps/shuttercrab"),
            Some(Path::new("/home/me/icon.png")),
        );
        assert!(entry.starts_with("[Desktop Entry]\nType=Application\nName=Shuttercrab\n"));
        assert!(entry.contains("\nExec=\"/home/me/My Apps/shuttercrab\" --background\n"));
        assert!(entry.contains("\nIcon=/home/me/icon.png\n"));
        assert!(!autostart_entry(Path::new("/s"), None).contains("Icon="));
    }
}
