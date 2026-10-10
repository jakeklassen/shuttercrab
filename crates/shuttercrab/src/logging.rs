//! Diagnostics (PRD §28): to `%LOCALAPPDATA%\Shuttercrab\logs\shuttercrab.log`,
//! and to stderr: everything in a debug build, only warnings and errors
//! while a release build starts, nothing after [`stop_terminal`]. The
//! previous run's log is kept as `shuttercrab.previous.log`. Logs never
//! contain pixels, clipboard contents or window titles.

use chrono::Local;
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::{
    fs::File,
    io::Write as _,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

/// The most verbose level that also goes to stderr, as a `LevelFilter`
/// number (0 = none).
static TERMINAL: AtomicUsize = AtomicUsize::new(if cfg!(debug_assertions) {
    LevelFilter::Trace as usize
} else {
    LevelFilter::Warn as usize
});

/// Stop writing to stderr; the log file keeps everything.
pub fn stop_terminal() {
    TERMINAL.store(LevelFilter::Off as usize, Ordering::Relaxed);
}

/// Libraries whose information lines are noise: D-Bus handshakes and the
/// like, on Linux.
const CHATTY: &[&str] = &["zbus", "tracing", "zed_xim"];

/// Whether a line from `target` at `level` goes in the log: Shuttercrab's
/// own down to debug, other libraries' down to information, the chatty
/// ones' only warnings and errors.
fn kept(target: &str, level: Level) -> bool {
    if target.starts_with("shuttercrab") {
        return true;
    }
    let chatty = CHATTY
        .iter()
        .any(|c| target == *c || target.starts_with(&format!("{c}::")));
    level <= if chatty { Level::Warn } else { Level::Info }
}

struct Logger {
    file: Option<Mutex<File>>,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        kept(metadata.target(), metadata.level())
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // GPUI's `log_err` sets no target outside its own repository; its
        // source location says where the error came from.
        let source = match (record.target(), record.file(), record.line()) {
            ("", Some(file), Some(line)) => {
                // Keep `crate/src/file.rs`, not the whole registry path.
                let parts: Vec<&str> = file.rsplit(['\\', '/']).take(3).collect();
                let short: Vec<&str> = parts.into_iter().rev().collect();
                format!("{}:{line}", short.join("/"))
            }
            (target, _, _) => target.to_string(),
        };
        let line = format!(
            "{} {:<5} {source}: {}",
            Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            record.level(),
            record.args()
        );
        if record.level() as usize <= TERMINAL.load(Ordering::Relaxed) {
            eprintln!("{line}");
        }
        if let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
        {
            let _ = writeln!(file, "{line}");
        }
    }

    fn flush(&self) {
        if let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
        {
            let _ = file.flush();
        }
    }
}

/// `%LOCALAPPDATA%\Shuttercrab\logs`.
pub fn default_dir() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join("Shuttercrab").join("logs"))
}

/// Start logging to stderr and, if it can be created, a file in `dir`.
/// Returns the log file's path.
pub fn init(dir: Option<&Path>) -> Option<PathBuf> {
    let path = dir.and_then(|dir| {
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join("shuttercrab.log");
        let _ = std::fs::rename(&path, dir.join("shuttercrab.previous.log"));
        Some(path)
    });
    let file = path
        .as_ref()
        .and_then(|p| File::create(p).ok())
        .map(Mutex::new);
    let written = file.is_some().then_some(path).flatten();
    if log::set_boxed_logger(Box::new(Logger { file })).is_ok() {
        log::set_max_level(LevelFilter::Debug);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chatty_libraries_log_only_warnings() {
        assert!(kept("shuttercrab::app", Level::Debug));
        assert!(kept("gpui_wgpu::wgpu_context", Level::Info));
        assert!(!kept("gpui_wgpu::wgpu_context", Level::Debug));
        assert!(!kept("zbus::connection::handshake::common", Level::Info));
        assert!(kept("zbus::proxy", Level::Warn));
        assert!(!kept("tracing::span", Level::Info));
        // A library that only starts with a chatty one's name is not it.
        assert!(kept("zbusy", Level::Info));
    }
}
