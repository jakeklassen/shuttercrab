//! Recording in a helper process (PRD §23). The hardware encoder's driver
//! keeps hundreds of megabytes of system memory for as long as the process
//! that used it lives (about 265 MB after a 4K recording). So Shuttercrab
//! records in a short-lived copy of itself, started with [`FLAG`], which
//! runs the recorder and exits when the recording ends, taking that memory
//! with it.
//!
//! The two talk in JSON lines: the app writes [`Request`]s to the helper's
//! standard input and reads [`Reply`]s from its standard output. The
//! helper's log lines come over standard error and go into the app's log.
//! If the app goes away, the helper's input ends, and it stops and finishes
//! the file as if asked.
//!
//! [`RecorderProcess`] has the same methods as the in-process
//! [`Recorder`], which [`serve`] runs inside the helper.

use anyhow::{Context, Result, anyhow};
use futures::channel::oneshot;
use serde::{Deserialize, Serialize};
use shuttercrab_capture::{
    MonitorId, PhysicalRect,
    record::{Interruption, RecordOptions, Recorder, RecordingSummary, Source},
};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Mutex, MutexGuard, PoisonError, mpsc},
    thread,
    time::Duration,
};

/// The command-line flag that makes `shuttercrab.exe` the recording helper.
pub const FLAG: &str = "--recorder";

/// What the app asks of the helper. The first request is always `Start`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Request {
    Start(Options),
    Pause,
    Resume,
    /// Switch the speakers' sound (`false`) or the microphone (`true`)
    /// on or off.
    SetSound {
        microphone: bool,
        on: bool,
    },
    DisplayGone,
    DisplayChanged,
    Stop,
}

/// [`RecordOptions`] on the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Options {
    monitor: u64,
    region: Option<(i32, i32, u32, u32)>,
    fps: u32,
    include_cursor: bool,
    system_sound: bool,
    microphone: bool,
    microphone_device: Option<String>,
    sound_track: bool,
    path: PathBuf,
}

/// What the helper says: that recording started (or could not), and how
/// it ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Reply {
    Started,
    Finished(Summary),
    Failed(String),
}

/// [`RecordingSummary`] on the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Summary {
    path: PathBuf,
    width: u32,
    height: u32,
    frames: u64,
    dropped_busy: u64,
    skipped_rate: u64,
    duration: Duration,
    paused: Duration,
    hardware_encoder: bool,
    sound: bool,
    interrupted: Option<Why>,
}

/// [`Interruption`] on the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Why {
    DisplayGone,
    DeviceLost,
    DiskFull,
    Failed(String),
}

impl From<RecordOptions> for Options {
    fn from(o: RecordOptions) -> Self {
        Self {
            monitor: o.monitor.0,
            region: o.region.map(|r| (r.x, r.y, r.width, r.height)),
            fps: o.fps,
            include_cursor: o.include_cursor,
            system_sound: o.system_sound,
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
            monitor: MonitorId(o.monitor),
            region: o.region.map(|(x, y, w, h)| PhysicalRect::new(x, y, w, h)),
            fps: o.fps,
            include_cursor: o.include_cursor,
            system_sound: o.system_sound,
            microphone: o.microphone,
            microphone_device: o.microphone_device,
            sound_track: o.sound_track,
            path: o.path,
        }
    }
}

impl From<RecordingSummary> for Summary {
    fn from(s: RecordingSummary) -> Self {
        Self {
            path: s.path,
            width: s.width,
            height: s.height,
            frames: s.frames,
            dropped_busy: s.dropped_busy,
            skipped_rate: s.skipped_rate,
            duration: s.duration,
            paused: s.paused,
            hardware_encoder: s.hardware_encoder,
            sound: s.sound,
            interrupted: s.interrupted.map(|i| match i {
                Interruption::DisplayGone => Why::DisplayGone,
                Interruption::DeviceLost => Why::DeviceLost,
                Interruption::DiskFull => Why::DiskFull,
                Interruption::Failed(m) => Why::Failed(m),
            }),
        }
    }
}

impl From<Summary> for RecordingSummary {
    fn from(s: Summary) -> Self {
        Self {
            path: s.path,
            width: s.width,
            height: s.height,
            frames: s.frames,
            dropped_busy: s.dropped_busy,
            skipped_rate: s.skipped_rate,
            duration: s.duration,
            paused: s.paused,
            hardware_encoder: s.hardware_encoder,
            sound: s.sound,
            interrupted: s.interrupted.map(|w| match w {
                Why::DisplayGone => Interruption::DisplayGone,
                Why::DeviceLost => Interruption::DeviceLost,
                Why::DiskFull => Interruption::DiskFull,
                Why::Failed(m) => Interruption::Failed(m),
            }),
            timing: None,
        }
    }
}

/// Write one message as a JSON line.
fn send(to: &mut impl Write, message: &impl Serialize) -> Result<()> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    to.write_all(line.as_bytes())?;
    to.flush()?;
    Ok(())
}

/// Windows' CREATE_NO_WINDOW: the helper gets no console window of its own.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A recording in progress in the helper process.
pub struct RecorderProcess {
    child: Child,
    /// The helper's input; closing it stops the recording.
    input: Mutex<Option<ChildStdin>>,
    /// How the recording ended, once it has.
    outcome: mpsc::Receiver<Result<RecordingSummary>>,
    /// Resolves when the helper has said how the recording ended, or died.
    ended: Option<oneshot::Receiver<()>>,
}

impl RecorderProcess {
    /// Start the helper and recording in it. Returns once the capture and
    /// the encoder are running.
    pub fn start(options: RecordOptions) -> Result<Self> {
        use std::os::windows::process::CommandExt as _;
        let exe = std::env::current_exe().context("could not find shuttercrab.exe")?;
        let mut child = Command::new(exe)
            .arg(FLAG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .context("could not start the recording process")?;
        let mut input = child
            .stdin
            .take()
            .context("no input to the recording process")?;
        let output = child
            .stdout
            .take()
            .context("no output from the recording process")?;
        let errors = child
            .stderr
            .take()
            .context("no log from the recording process")?;
        thread::Builder::new()
            .name("shuttercrab-recorder-log".into())
            .spawn(move || forward_logs(errors))?;
        send(&mut input, &Request::Start(options.into()))?;

        let (started_tx, started_rx) = mpsc::channel();
        let (outcome_tx, outcome_rx) = mpsc::channel();
        let (ending, ended) = oneshot::channel::<()>();
        thread::Builder::new()
            .name("shuttercrab-recorder-replies".into())
            .spawn(move || {
                // Dropped when this thread ends, which resolves `ended`.
                let _ending = ending;
                read_replies(output, started_tx, outcome_tx);
            })?;

        let started = started_rx
            .recv()
            .unwrap_or_else(|_| Err(anyhow!("the recording process stopped")));
        if let Err(e) = started {
            // Closing its input makes a helper that is still running exit.
            drop(input);
            let _ = child.wait();
            return Err(e);
        }
        Ok(Self {
            child,
            input: Mutex::new(Some(input)),
            outcome: outcome_rx,
            ended: Some(ended),
        })
    }

    /// The helper's input. A panic while it was locked left at worst a
    /// partial line, which the helper logs and skips, so a poisoned lock is
    /// used anyway: giving up on the pipe would leave the helper recording.
    fn input(&self) -> MutexGuard<'_, Option<ChildStdin>> {
        self.input.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn request(&self, request: Request) {
        if let Some(input) = self.input().as_mut()
            && let Err(e) = send(input, &request)
        {
            // The helper has gone; `ended` says so.
            log::debug!("could not reach the recorder: {e:#}");
        }
    }

    pub fn pause(&self) {
        self.request(Request::Pause);
    }

    pub fn resume(&self) {
        self.request(Request::Resume);
    }

    /// Switch `source` on or off; see [`Recorder::set_sound`].
    pub fn set_sound(&self, source: Source, on: bool) {
        let microphone = source == Source::Microphone;
        self.request(Request::SetSound { microphone, on });
    }

    /// The recorded display is gone; see [`Recorder::display_gone`].
    pub fn display_gone(&self) {
        self.request(Request::DisplayGone);
    }

    /// The displays changed; see [`Recorder::display_changed`].
    pub fn display_changed(&self) {
        self.request(Request::DisplayChanged);
    }

    /// A future that resolves when the recording ends, by
    /// [`RecorderProcess::stop`] or by itself; see [`Recorder::ended`].
    /// `None` after the first call.
    pub fn ended(&mut self) -> Option<impl std::future::Future<Output = ()> + 'static> {
        let ended = self.ended.take()?;
        Some(async move {
            let _ = ended.await;
        })
    }

    /// Stop, finish the file, and report what was recorded. The helper
    /// exits.
    pub fn stop(mut self) -> Result<RecordingSummary> {
        self.close();
        let outcome = self
            .outcome
            .recv()
            .unwrap_or_else(|_| Err(anyhow!("the recording process ended unexpectedly")));
        let _ = self.child.wait();
        outcome
    }

    /// Ask the helper to stop and close its input, once.
    fn close(&mut self) {
        let input = self.input().take();
        if let Some(mut input) = input {
            let _ = send(&mut input, &Request::Stop);
        }
    }
}

impl Drop for RecorderProcess {
    /// Like dropping a [`Recorder`]: stop, and wait for the file.
    fn drop(&mut self) {
        self.close();
        let _ = self.child.wait();
    }
}

/// Read the helper's [`Reply`]s until its output ends. The first says
/// whether the recording started, sent on `started`; the last says how it
/// ended, sent on `outcome`. A helper that goes away first is a failure.
///
/// A read error ends the replies like the helper exiting does: no more will
/// come either way. A send fails only when nobody is waiting any more (the
/// app gave up on the recording), so those errors are ignored.
fn read_replies(
    output: ChildStdout,
    started: mpsc::Sender<Result<()>>,
    outcome: mpsc::Sender<Result<RecordingSummary>>,
) {
    let mut started = Some(started);
    let mut ended = None;
    for line in BufReader::new(output).lines().map_while(Result::ok) {
        match serde_json::from_str::<Reply>(&line) {
            Ok(Reply::Started) => {
                if let Some(started) = started.take() {
                    let _ = started.send(Ok(()));
                }
            }
            Ok(Reply::Failed(e)) => match started.take() {
                Some(started) => {
                    let _ = started.send(Err(anyhow!(e)));
                }
                None => ended = Some(Err(anyhow!(e))),
            },
            Ok(Reply::Finished(summary)) => ended = Some(Ok(summary.into())),
            Err(e) => log::warn!("unreadable reply from the recorder: {e}"),
        }
    }
    let gone = || anyhow!("the recording process ended unexpectedly");
    if let Some(started) = started {
        let _ = started.send(Err(gone()));
    }
    let _ = outcome.send(ended.unwrap_or_else(|| Err(gone())));
}

/// The helper's log, from its standard error, into the app's log until the
/// helper exits. A read error ends it too: the log is only for diagnosis.
fn forward_logs(errors: ChildStderr) {
    for line in BufReader::new(errors).lines().map_while(Result::ok) {
        forward_log(&line);
    }
}

/// One of the helper's log lines, `LEVEL\tsource\tmessage`, into the app's
/// log.
fn forward_log(line: &str) {
    let mut parts = line.splitn(3, '\t');
    let (level, source, message) = match (parts.next(), parts.next(), parts.next()) {
        (Some(level), Some(source), Some(message)) => {
            (level.parse().unwrap_or(log::Level::Info), source, message)
        }
        _ => (log::Level::Info, "", line),
    };
    log::log!(target: "shuttercrab::recorder", level, "{source}: {message}");
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

/// Run as the recording helper: read [`Request`]s from standard input,
/// record, and write [`Reply`]s to standard output. Returns the exit code.
pub fn serve() -> i32 {
    if log::set_boxed_logger(Box::new(HelperLogger)).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
    let mut out = std::io::stdout();
    let mut first = String::new();
    let read = std::io::stdin().read_line(&mut first);
    let options = match read.map(|_| serde_json::from_str(first.trim())) {
        Ok(Ok(Request::Start(options))) => options,
        _ => {
            log::error!("the recorder was not asked to start");
            return 2;
        }
    };
    let mut recorder = match Recorder::start(options.into()) {
        Ok(recorder) => recorder,
        Err(e) => {
            let _ = send(&mut out, &Reply::Failed(format!("{e:#}")));
            return 1;
        }
    };
    if send(&mut out, &Reply::Started).is_err() {
        return 1;
    }

    enum Next {
        Asked(Request),
        Ended,
    }
    let (tx, rx) = mpsc::channel();
    let asked = tx.clone();
    thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            match serde_json::from_str(&line) {
                Ok(request) => {
                    if asked.send(Next::Asked(request)).is_err() {
                        return;
                    }
                }
                Err(e) => log::warn!("unreadable request: {e}"),
            }
        }
        // The app closed our input, or went away: stop.
        let _ = asked.send(Next::Asked(Request::Stop));
    });
    if let Some(ended) = recorder.ended() {
        thread::spawn(move || {
            futures::executor::block_on(ended);
            let _ = tx.send(Next::Ended);
        });
    }
    while let Ok(next) = rx.recv() {
        match next {
            Next::Asked(Request::Pause) => recorder.pause(),
            Next::Asked(Request::Resume) => recorder.resume(),
            Next::Asked(Request::SetSound { microphone, on }) => {
                let source = if microphone {
                    Source::Microphone
                } else {
                    Source::System
                };
                recorder.set_sound(source, on);
            }
            Next::Asked(Request::DisplayGone) => recorder.display_gone(),
            Next::Asked(Request::DisplayChanged) => recorder.display_changed(),
            Next::Asked(Request::Start(_)) => log::warn!("the recorder is already recording"),
            Next::Asked(Request::Stop) | Next::Ended => break,
        }
    }
    crate::heap::log_memory("in the recording process, before it finishes");
    let reply = match recorder.stop() {
        Ok(summary) => Reply::Finished(summary.into()),
        Err(e) => Reply::Failed(format!("{e:#}")),
    };
    match send(&mut out, &reply) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_replies_round_trip_as_json_lines() {
        let options = RecordOptions {
            monitor: MonitorId(0x1_0001),
            region: Some(PhysicalRect::new(-12, 34, 1280, 720)),
            fps: 60,
            include_cursor: true,
            system_sound: true,
            microphone: true,
            microphone_device: Some("{0.0.1.00000000}.{mic}".into()),
            sound_track: true,
            path: PathBuf::from(r"C:\Videos\Shuttercrab\Recording 1.mp4.partial"),
        };
        let mut bytes = Vec::new();
        send(&mut bytes, &Request::Start(options.clone().into())).unwrap();
        send(&mut bytes, &Request::Pause).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "one message per line: {text}");
        let Request::Start(back) = serde_json::from_str(lines[0]).unwrap() else {
            panic!("expected Start");
        };
        let back = RecordOptions::from(back);
        assert_eq!(back.monitor, options.monitor);
        assert_eq!(back.region, options.region);
        assert_eq!(back.path, options.path);
        assert!(back.system_sound && back.microphone && back.sound_track);
        assert_eq!(back.microphone_device, options.microphone_device);
        assert_eq!(
            serde_json::from_str::<Request>(lines[1]).unwrap(),
            Request::Pause
        );

        let summary = RecordingSummary {
            path: options.path,
            width: 1280,
            height: 720,
            frames: 1800,
            dropped_busy: 1,
            skipped_rate: 2,
            duration: Duration::from_millis(60_010),
            paused: Duration::from_secs(10),
            hardware_encoder: true,
            sound: true,
            interrupted: Some(Interruption::Failed("disk".into())),
            timing: None,
        };
        let line = serde_json::to_string(&Reply::Finished(summary.clone().into())).unwrap();
        let Reply::Finished(back) = serde_json::from_str(&line).unwrap() else {
            panic!("expected Finished");
        };
        let back = RecordingSummary::from(back);
        assert_eq!(back.duration, summary.duration);
        assert_eq!(back.interrupted, summary.interrupted);
        assert_eq!(back.frames, 1800);
    }

    #[test]
    fn helper_log_lines_keep_their_level_and_source() {
        // A line that is not in the helper's format is still kept.
        forward_log("WARN\tshuttercrab_capture::record\trecording interrupted");
        forward_log("not tab separated");
        let mut parts = "ERROR\tsrc\ta\tb".splitn(3, '\t');
        assert_eq!(parts.next(), Some("ERROR"));
        assert_eq!(parts.nth(1), Some("a\tb"));
    }
}
