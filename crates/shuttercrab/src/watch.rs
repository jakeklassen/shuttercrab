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
    let input = match Input::online(&link, shuttercrab_stream::MEET, Path::Relay) {
        Ok(input) => input,
        Err(e) => {
            log::error!("could not join the stream: {e:#}");
            tell(
                "Shuttercrab",
                &format!("Could not join the stream.\n\n{e:#}"),
            );
            return 1;
        }
    };
    let options = WatchOptions {
        monitor: None,
        muted: false,
        title: "Shuttercrab".into(),
    };
    match shuttercrab_stream::watch(input, &options) {
        Ok(()) => 0,
        Err(e) => {
            log::error!("the stream could not play: {e:#}");
            tell("Shuttercrab", &format!("The stream could not play.\n\n{e:#}"));
            1
        }
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
