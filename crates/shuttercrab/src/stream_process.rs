//! Streaming in a helper process (issue #66), as recording is (see
//! `recorder_process`): `shuttercrab.exe` started with [`FLAG`] opens a
//! room on the meeting point, says the link to share, waits for a viewer,
//! and streams to it until asked to stop or the viewer leaves. The
//! hardware encoder's memory goes with the helper, and the app never waits
//! on the network.
//!
//! The two talk in JSON lines, as the recording helper does: [`Request`]s
//! in on standard input, [`Reply`]s out on standard output, the log on
//! standard error. If the app goes away, the helper's input ends and it
//! stops.

use crate::recorder_process::{Options, forward_logs, send};
use anyhow::{Context, Result, anyhow};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde::{Deserialize, Serialize};
use shuttercrab_capture::record::{RecordOptions, Source};
use shuttercrab_platform::process::{Process, Spawned, spawn_quiet};
use shuttercrab_stream::Quality;
use std::{
    fs::File,
    io::{BufRead, BufReader},
    sync::{Mutex, MutexGuard, PoisonError, mpsc},
    thread,
};

/// The command-line flag that makes `shuttercrab.exe` the streaming helper.
pub const FLAG: &str = "--streamer";

/// What to stream, and how to reach the meeting point.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamStart {
    options: Options,
    /// What is recorded, physical pixels.
    source: (u32, u32),
    quality: Quality,
    meet: String,
    /// Never logged.
    host_key: String,
}

impl std::fmt::Debug for StreamStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamStart")
            .field("options", &self.options)
            .field("source", &self.source)
            .field("quality", &self.quality)
            .field("meet", &self.meet)
            .finish_non_exhaustive()
    }
}

impl StreamStart {
    pub fn new(
        options: RecordOptions,
        source: (u32, u32),
        quality: Quality,
        host_key: String,
    ) -> Self {
        Self {
            options: options.into(),
            source,
            quality,
            meet: shuttercrab_stream::MEET.into(),
            host_key,
        }
    }
}

/// What the app asks of the helper. The first request is always `Start`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Request {
    Start(StreamStart),
    Quality(Quality),
    /// Switch the speakers' sound (`false`) or the microphone (`true`) on
    /// or off.
    SetSound {
        microphone: bool,
        on: bool,
    },
    Stop,
}

/// What the helper says.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Reply {
    /// The room is open: the link to share.
    Link(String),
    /// Opening the room failed, and why.
    Failed(String),
    /// A viewer came, and the pictures go to it as `codec`.
    Viewer { codec: String, relayed: bool },
    /// The last second.
    Stats(Stats),
    /// The stream is over, and why.
    Ended(String),
}

/// How the stream went over the last second.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub quality: Quality,
    pub pictures_per_second: f64,
    pub megabits_per_second: f64,
    pub round_trip_ms: Option<u64>,
    pub lost_percent: Option<f64>,
}

/// What the app hears of the stream once it is up.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    Viewer {
        codec: String,
        relayed: bool,
    },
    Stats(Stats),
    /// Over, and why; nothing follows.
    Ended(String),
}

/// A stream in the helper process.
pub struct StreamProcess {
    child: Process,
    input: Mutex<Option<File>>,
}

impl StreamProcess {
    /// Start the helper and open the room. Returns once the room is open,
    /// with the link to share and what happens next. Blocks meanwhile.
    pub fn start(start: StreamStart) -> Result<(Self, String, UnboundedReceiver<StreamEvent>)> {
        let exe = std::env::current_exe().context("could not find shuttercrab.exe")?;
        let Spawned {
            process: child,
            stdin: mut input,
            stdout: output,
            stderr: errors,
        } = spawn_quiet(&exe, &[FLAG]).context("could not start the streaming process")?;
        thread::Builder::new()
            .name("shuttercrab-streamer-log".into())
            .spawn(move || forward_logs(errors))?;
        send(&mut input, &Request::Start(start))?;
        let (opened_tx, opened_rx) = mpsc::channel();
        let (events_tx, events_rx) = unbounded();
        thread::Builder::new()
            .name("shuttercrab-streamer-replies".into())
            .spawn(move || read_replies(output, opened_tx, events_tx))?;
        let opened = opened_rx
            .recv()
            .unwrap_or_else(|_| Err(anyhow!("the streaming process stopped")));
        match opened {
            Ok(link) => Ok((
                Self {
                    child,
                    input: Mutex::new(Some(input)),
                },
                link,
                events_rx,
            )),
            Err(e) => {
                drop(input);
                let _ = child.wait();
                Err(e)
            }
        }
    }

    fn input(&self) -> MutexGuard<'_, Option<File>> {
        self.input.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn request(&self, request: &Request) {
        if let Some(input) = self.input().as_mut()
            && let Err(e) = send(input, request)
        {
            log::debug!("could not reach the streamer: {e:#}");
        }
    }

    pub fn set_quality(&self, quality: Quality) {
        self.request(&Request::Quality(quality));
    }

    pub fn set_sound(&self, source: Source, on: bool) {
        let microphone = source == Source::Microphone;
        self.request(&Request::SetSound { microphone, on });
    }

    /// Stop streaming; the helper exits. Blocks until it has.
    pub fn stop(self) {
        drop(self);
    }
}

impl Drop for StreamProcess {
    fn drop(&mut self) {
        if let Some(mut input) = self.input().take() {
            let _ = send(&mut input, &Request::Stop);
        }
        let _ = self.child.wait();
    }
}

/// Read the helper's replies: the first says whether the room opened, sent
/// on `opened`; the rest go to `events`, ending with [`StreamEvent::Ended`].
fn read_replies(
    output: File,
    opened: mpsc::Sender<Result<String>>,
    events: UnboundedSender<StreamEvent>,
) {
    let mut opened = Some(opened);
    let mut ended = false;
    for line in BufReader::new(output).lines().map_while(Result::ok) {
        let event = match serde_json::from_str::<Reply>(&line) {
            Ok(Reply::Link(link)) => {
                if let Some(opened) = opened.take() {
                    let _ = opened.send(Ok(link));
                }
                continue;
            }
            Ok(Reply::Failed(why)) => {
                if let Some(opened) = opened.take() {
                    let _ = opened.send(Err(anyhow!(why)));
                }
                continue;
            }
            Ok(Reply::Viewer { codec, relayed }) => StreamEvent::Viewer { codec, relayed },
            Ok(Reply::Stats(stats)) => StreamEvent::Stats(stats),
            Ok(Reply::Ended(why)) => {
                ended = true;
                StreamEvent::Ended(why)
            }
            Err(e) => {
                log::warn!("unreadable reply from the streamer: {e}");
                continue;
            }
        };
        let _ = events.unbounded_send(event);
    }
    if let Some(opened) = opened {
        let _ = opened.send(Err(anyhow!("the streaming process ended unexpectedly")));
    }
    if !ended {
        let _ = events.unbounded_send(StreamEvent::Ended(
            "The streaming process ended unexpectedly.".into(),
        ));
    }
}

#[cfg(windows)]
pub use helper::serve;

/// The helper's side, which streams.
#[cfg(windows)]
mod helper {
    use super::{Reply, Request, Stats, send};
    use shuttercrab_capture::{
        live::{SoundCodec, VideoCodec},
        record::{RecordOptions, Source},
    };
    use shuttercrab_stream::{Path, Quality, Room, Stream, Viewer};
    use std::{io::Stdout, sync::mpsc, thread, time::Duration};

    /// What happens next, for the helper's loop.
    enum Next {
        Asked(Request),
        Came(anyhow::Result<Viewer>),
    }

    /// Run as the streaming helper: read [`Request`]s from standard input,
    /// stream, and write [`Reply`]s to standard output. Returns the exit
    /// code.
    pub fn serve() -> i32 {
        if log::set_boxed_logger(Box::new(crate::recorder_process::HelperLogger)).is_ok() {
            log::set_max_level(log::LevelFilter::Info);
        }
        let mut out = std::io::stdout();
        let mut first = String::new();
        let read = std::io::stdin().read_line(&mut first);
        let Ok(Ok(Request::Start(start))) = read.map(|_| serde_json::from_str(first.trim())) else {
            log::error!("the streamer was not asked to start");
            return 2;
        };
        let room = match Room::open(&start.meet, &start.host_key, Path::Relay) {
            Ok(room) => room,
            Err(e) => {
                let _ = send(&mut out, &Reply::Failed(format!("{e:#}")));
                return 1;
            }
        };
        if send(&mut out, &Reply::Link(room.link().to_string())).is_err() {
            return 1;
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
        thread::spawn(move || {
            let _ = tx.send(Next::Came(room.accept()));
        });

        let mut options = RecordOptions::from(start.options);
        let mut quality = start.quality;
        let viewer = match wait_for_viewer(&rx, &mut options, &mut quality) {
            Ok(viewer) => viewer,
            Err(why) => {
                let _ = send(&mut out, &Reply::Ended(why));
                return 0;
            }
        };
        let relayed = viewer.connection().is_some_and(|c| c.relayed());
        let stream = match Stream::start(
            options,
            start.source,
            viewer,
            quality,
            (VideoCodec::Av1, SoundCodec::Opus),
        ) {
            Ok(stream) => stream,
            Err(e) => {
                let _ = send(&mut out, &Reply::Ended(format!("Could not stream: {e:#}")));
                return 1;
            }
        };
        let codec = shuttercrab_stream::describe(stream.video()).to_string();
        log::info!("streaming as {codec}");
        if send(&mut out, &Reply::Viewer { codec, relayed }).is_err() {
            return 1;
        }
        let why = run(stream, &rx, &mut out);
        let _ = send(&mut out, &Reply::Ended(why));
        0
    }

    /// Wait for a viewer. Choices made meanwhile wait for the stream, in
    /// `options` and `quality`. Why not, if one never comes.
    fn wait_for_viewer(
        rx: &mpsc::Receiver<Next>,
        options: &mut RecordOptions,
        quality: &mut Quality,
    ) -> Result<Viewer, String> {
        loop {
            match rx.recv() {
                Ok(Next::Came(Ok(viewer))) => return Ok(viewer),
                Ok(Next::Came(Err(e))) => return Err(format!("{e:#}")),
                Ok(Next::Asked(Request::Quality(q))) => *quality = q,
                Ok(Next::Asked(Request::SetSound { microphone, on })) => {
                    if microphone {
                        options.microphone = on;
                    } else {
                        options.system_sound = on;
                    }
                }
                Ok(Next::Asked(Request::Stop)) | Err(_) => return Err("Stopped.".into()),
                Ok(Next::Asked(Request::Start(_))) => log::warn!("already streaming"),
            }
        }
    }

    /// Stream until asked to stop or the viewer leaves, with a status a
    /// second: why it ended.
    fn run(mut stream: Stream, rx: &mpsc::Receiver<Next>, out: &mut Stdout) -> String {
        let why = loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(Next::Asked(Request::Quality(q))) => {
                    if let Err(e) = stream.set_quality(q) {
                        break format!("Could not switch to {}: {e:#}", q.name());
                    }
                }
                Ok(Next::Asked(Request::SetSound { microphone, on })) => {
                    let source = if microphone {
                        Source::Microphone
                    } else {
                        Source::System
                    };
                    stream.set_sound(source, on);
                }
                Ok(Next::Asked(Request::Stop)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break "Stopped.".to_string();
                }
                Ok(Next::Asked(Request::Start(_)) | Next::Came(_)) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if stream.gone() {
                        break "The viewer left.".to_string();
                    }
                    let s = stream.stats();
                    let stats = Stats {
                        quality: s.quality,
                        pictures_per_second: s.pictures_per_second,
                        megabits_per_second: s.megabits_per_second,
                        round_trip_ms: s.round_trip.map(|d| d.as_millis() as u64),
                        lost_percent: s.lost,
                    };
                    if send(out, &Reply::Stats(stats)).is_err() {
                        break "Stopped.".to_string();
                    }
                }
            }
        };
        log::info!(
            "stream over after {:.0} s: {why}",
            stream.elapsed().as_secs_f64()
        );
        if let Err(e) = stream.stop() {
            log::warn!("the recorder did not stop cleanly: {e:#}");
        }
        why
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shuttercrab_capture::{MonitorId, PhysicalRect};
    use std::path::PathBuf;

    #[test]
    fn the_host_key_is_sent_but_never_shown() {
        let options = RecordOptions {
            monitor: MonitorId::from_raw(1),
            region: Some(PhysicalRect::new(0, 0, 1920, 1080)),
            window: None,
            fps: 60,
            include_cursor: true,
            system_sound: true,
            sound_process: None,
            microphone: false,
            microphone_device: None,
            sound_track: true,
            path: PathBuf::new(),
        };
        let start = StreamStart::new(options, (1920, 1080), Quality::P1080, "key-123".into());
        let line = serde_json::to_string(&Request::Start(start.clone())).unwrap();
        let Request::Start(back) = serde_json::from_str(&line).unwrap() else {
            panic!("expected Start");
        };
        assert_eq!(back, start);
        assert!(!format!("{start:?}").contains("key-123"));
        let reply = serde_json::to_string(&Reply::Viewer {
            codec: "AV1, 10 bits".into(),
            relayed: true,
        })
        .unwrap();
        assert!(matches!(
            serde_json::from_str(&reply).unwrap(),
            Reply::Viewer { relayed: true, .. }
        ));
    }
}
