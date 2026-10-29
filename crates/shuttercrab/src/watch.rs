//! Watching a stream (issue #66): `shuttercrab.exe --watch LINK` opens a
//! window that plays the stream the link is for, in a process of its own,
//! beside the app or without it. The app's Watch box starts one, and so
//! does Windows for a `shuttercrab://` link clicked anywhere, once the
//! installer has registered them.

use anyhow::{Context, Result};
use std::process::Command;

/// The command-line flag that makes `shuttercrab.exe` a viewer.
pub const FLAG: &str = "--watch";

/// The links Shuttercrab opens.
pub const SCHEME: &str = "shuttercrab";

/// Whether `text` looks like a stream link, to offer it from the
/// clipboard.
pub fn is_link(text: &str) -> bool {
    shuttercrab_stream::Link::parse(text.trim()).is_ok()
}

/// Open a viewer for `link` in its own process.
pub fn open(link: &str) -> Result<()> {
    let exe = std::env::current_exe().context("could not find shuttercrab.exe")?;
    Command::new(exe)
        .arg(FLAG)
        .arg(link.trim())
        .spawn()
        .context("could not start the viewer")?;
    Ok(())
}

/// Run as a viewer: play the stream named on the command line until the
/// window is closed. Returns the exit code.
#[cfg(windows)]
pub fn serve() -> i32 {
    use shuttercrab_stream::{Input, Link, Path, WatchOptions, tell};

    let _ = crate::logging::init_as(crate::logging::default_dir().as_deref(), "viewer");
    let given = std::env::args().nth(2).unwrap_or_default();
    let link = match Link::parse(given.trim()) {
        Ok(link) => link,
        Err(e) => {
            log::error!("not a stream link: {e:#}");
            tell(
                "Shuttercrab",
                "That is not a Shuttercrab stream link. Links start shuttercrab://watch/.",
            );
            return 2;
        }
    };
    log::info!("joining {link:?}");
    let options = WatchOptions {
        monitor: None,
        muted: false,
        volume: saved_volume(),
        remember_volume: Some(remember_volume),
        title: "Shuttercrab".into(),
    };
    let join = move || Input::online(&link, shuttercrab_stream::MEET, Path::Relay);
    // Errors once the window is open are shown in it; this is for a window
    // that could not open.
    match shuttercrab_stream::watch(join, &options) {
        Ok(()) => 0,
        Err(e) => {
            log::error!("the stream could not play: {e:#}");
            tell(
                "Shuttercrab",
                &format!("The stream could not play.\n\n{e:#}"),
            );
            1
        }
    }
}

/// Where the viewer keeps its volume: beside the settings, in a file of its
/// own, as it runs beside the app and must not write the app's.
#[cfg(windows)]
fn volume_file() -> Option<std::path::PathBuf> {
    let dir = match std::env::var_os("SHUTTERCRAB_DATA_DIR") {
        Some(dir) => dir.into(),
        None => crate::settings::default_path()?.parent()?.to_path_buf(),
    };
    Some(dir.join("viewer-volume.txt"))
}

/// The volume the viewer last played at, if it has played.
#[cfg(windows)]
fn saved_volume() -> Option<u32> {
    std::fs::read_to_string(volume_file()?)
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Keep percent as the volume to play at next time.
#[cfg(windows)]
fn remember_volume(percent: u32) {
    let Some(file) = volume_file() else {
        return;
    };
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&file, percent.to_string()) {
        log::warn!("could not keep the volume: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_stream_links_are_offered() {
        assert!(!is_link("https://example.com"));
        assert!(!is_link("shuttercrab://watch/"));
        assert!(!is_link(""));
    }
}
