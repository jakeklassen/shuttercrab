//! What Shuttercrab's helper processes share. Recording and streaming each
//! run in a short-lived copy of `shuttercrab.exe`, started with a flag of
//! their own (see `recorder_process` and `stream_process`).
//!
//! The app and a helper talk in JSON lines: requests go to the helper's
//! standard input and replies come back on its standard output. The
//! helper's log lines come over standard error and go into the app's log.

use anyhow::Result;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use shuttercrab_capture::{MonitorId, PhysicalRect, record::RecordOptions};
use shuttercrab_types::WindowId;
use std::{
    fs::File,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    sync::mpsc,
    thread,
};

/// [`RecordOptions`] on the wire. Both helpers record: the streamer
/// streams what it records.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Options {
    monitor: u64,
    region: Option<(i32, i32, u32, u32)>,
    window: Option<u64>,
    fps: u32,
    include_cursor: bool,
    system_sound: bool,
    sound_process: Option<u32>,
    microphone: bool,
    microphone_device: Option<String>,
    sound_track: bool,
    path: PathBuf,
}

impl From<RecordOptions> for Options {
    fn from(o: RecordOptions) -> Self {
        Self {
            monitor: o.monitor.raw(),
            region: o.region.map(|r| (r.x, r.y, r.width, r.height)),
            window: o.window.map(WindowId::raw),
            fps: o.fps,
            include_cursor: o.include_cursor,
            system_sound: o.system_sound,
            sound_process: o.sound_process,
            microphone: o.microphone,
            microphone_device: o.microphone_device,
            sound_track: o.sound_track,
            path: o.path,
        }
    }
}

impl From<Options> for RecordOptions {
    fn from(o: Options) -> Self {
        Self {
            monitor: MonitorId::from_raw(o.monitor),
            region: o.region.map(|(x, y, w, h)| PhysicalRect::new(x, y, w, h)),
            window: o.window.map(WindowId::from_raw),
            fps: o.fps,
            include_cursor: o.include_cursor,
            system_sound: o.system_sound,
            sound_process: o.sound_process,
            microphone: o.microphone,
            microphone_device: o.microphone_device,
            sound_track: o.sound_track,
            path: o.path,
        }
    }
}

/// Write one message as a JSON line.
pub(crate) fn send(to: &mut impl Write, message: &impl Serialize) -> Result<()> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    to.write_all(line.as_bytes())?;
    to.flush()?;
    Ok(())
}

/// The helper's log, from its standard error, into the app's log under
/// `target` until the helper exits. A read error ends it too: the log is
/// only for diagnosis.
pub(crate) fn forward_logs(errors: File, target: &str) {
    for line in BufReader::new(errors).lines().map_while(Result::ok) {
        forward_log(&line, target);
    }
}

/// One of the helper's log lines, `LEVEL\tsource\tmessage`, into the app's
/// log under `target`.
fn forward_log(line: &str, target: &str) {
    let mut parts = line.splitn(3, '\t');
    let (level, source, message) = match (parts.next(), parts.next(), parts.next()) {
        (Some(level), Some(source), Some(message)) => {
            (level.parse().unwrap_or(log::Level::Info), source, message)
        }
        _ => (log::Level::Info, "", line),
    };
    log::log!(target: target, level, "{source}: {message}");
}

/// In the helper: the app's first request, from standard input. `None` if
/// the input ended first or the line could not be read.
pub(crate) fn first_request<R: DeserializeOwned>() -> Option<R> {
    let mut first = String::new();
    std::io::stdin().read_line(&mut first).ok()?;
    serde_json::from_str(first.trim()).ok()
}

/// In the helper: read the app's requests from standard input on a thread
/// of their own, sending each to `next` as `asked(request)`. When the input
/// ends, because the app closed it or went away, sends `asked(stop)`.
///
/// A line that is not a request is logged and skipped. The thread ends
/// early if nobody listens on `next` any more.
pub(crate) fn read_requests<R, N>(next: mpsc::Sender<N>, asked: fn(R) -> N, stop: R)
where
    R: DeserializeOwned + Send + 'static,
    N: Send + 'static,
{
    thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            match serde_json::from_str(&line) {
                Ok(request) => {
                    if next.send(asked(request)).is_err() {
                        return;
                    }
                }
                Err(e) => log::warn!("unreadable request: {e}"),
            }
        }
        let _ = next.send(asked(stop));
    });
}

/// In the helper: send its log, as detailed as `max`, to the app, which
/// reads it with [`forward_logs`].
pub(crate) fn log_to_app(max: log::LevelFilter) {
    if log::set_boxed_logger(Box::new(HelperLogger)).is_ok() {
        log::set_max_level(max);
    }
}

/// The helper's logger: one tab-separated line per message on standard
/// error, for [`forward_log`].
struct HelperLogger;

impl log::Log for HelperLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info || metadata.target().starts_with("shuttercrab")
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let message = record.args().to_string().replace(['\n', '\r'], " ");
            eprintln!("{}\t{}\t{message}", record.level(), record.target());
        }
    }

    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_log_lines_keep_their_level_and_source() {
        // A line that is not in the helper's format is still kept.
        forward_log(
            "WARN\tshuttercrab_capture::record\trecording interrupted",
            "shuttercrab::recorder",
        );
        forward_log("not tab separated", "shuttercrab::recorder");
        let mut parts = "ERROR\tsrc\ta\tb".splitn(3, '\t');
        assert_eq!(parts.next(), Some("ERROR"));
        assert_eq!(parts.nth(1), Some("a\tb"));
    }
}
