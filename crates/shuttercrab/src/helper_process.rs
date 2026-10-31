//! What Shuttercrab's helper processes share. Recording and streaming each
//! run in a short-lived copy of `shuttercrab.exe`, started with a flag of
//! their own (see `recorder_process` and `stream_process`).
//!
//! The app and a helper talk in JSON lines: requests go to the helper's
//! standard input and replies come back on its standard output. The
//! helper's log lines come over standard error and go into the app's log.

use anyhow::Result;
use serde::Serialize;
use std::{
    fs::File,
    io::{BufRead, BufReader, Write},
};

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
