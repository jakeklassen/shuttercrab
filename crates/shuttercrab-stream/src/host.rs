//! The streaming side: a viewer to send to, and the recording sent.

use crate::{PICTURES, Path, Quality, SOUND, playable};
use anyhow::{Context, Result};
use shuttercrab_capture::{
    live::{self, LiveFormat, LiveFrame, SoundCodec, Track, VideoCodec},
    record::{Live, LiveSink, RecordOptions, Recorder, RecordingSummary, Source},
};
use shuttercrab_connect::{Connection, Hosting, Link};
use std::{
    io::{BufWriter, Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Where the frames go.
type Out = Box<dyn Write + Send>;

/// Where the frames go, for the threads that send them.
type SharedOut = Arc<Mutex<BufWriter<Out>>>;

fn shared(out: Out) -> SharedOut {
    Arc::new(Mutex::new(BufWriter::new(out)))
}

/// A viewer connected: where the pictures go, the sound if it goes apart
/// from them, and the codecs it can play.
pub struct Viewer {
    pictures: SharedOut,
    sound: Option<SharedOut>,
    plays: u8,
    connection: Option<Connection>,
}

impl Viewer {
    /// The connection, if the viewer is online.
    pub fn connection(&self) -> Option<&Connection> {
        self.connection.as_ref()
    }
}

/// A viewer on this PC, from a watcher given `127.0.0.1:port`.
pub fn wait_locally(port: u16) -> Result<Viewer> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let (socket, viewer) = listener.accept()?;
    socket.set_nodelay(true)?;
    log::info!("viewer {viewer} connected");
    let mut plays = [0];
    (&socket).read_exact(&mut plays)?;
    Ok(Viewer {
        pictures: shared(Box::new(socket)),
        sound: None,
        plays: plays[0],
        connection: None,
    })
}

/// A room on the meeting point to stream from, with a link to share.
pub struct Room(Hosting);

impl Room {
    /// Open a room at `meet` with `host_key`, reached by `path`.
    pub fn open(meet: &str, host_key: &str, path: Path) -> Result<Self> {
        Ok(Self(shuttercrab_connect::host(meet, host_key, path)?))
    }

    /// What to share: whoever has it can watch.
    pub fn link(&self) -> &Link {
        self.0.link()
    }

    /// Wait for a viewer and connect to it. The sound has a stream of its
    /// own, sent first, so a large picture cannot hold it up. The codecs
    /// the viewer can play come on a stream from it.
    pub fn accept(self) -> Result<Viewer> {
        let connection = self.0.accept()?;
        log::info!(
            "viewer at {} connected {}, {} ms there and back",
            connection.remote(),
            if connection.relayed() {
                "through the relay"
            } else {
                "directly"
            },
            connection.round_trip().as_millis()
        );
        let mut plays = [0];
        connection.receive()?.read_exact(&mut plays)?;
        let mut pictures = connection.send(0)?;
        pictures.write_all(&[PICTURES])?;
        let mut sound = connection.send(1)?;
        sound.write_all(&[SOUND])?;
        Ok(Viewer {
            pictures: shared(Box::new(pictures)),
            sound: Some(shared(Box::new(sound))),
            plays: plays[0],
            connection: Some(connection),
        })
    }
}

/// What has gone to the viewer, for [`Stream::stats`].
#[derive(Default)]
struct Sent {
    pictures: u64,
    sound: u64,
    bytes: u64,
}

/// How a stream has gone since the last [`Stream::stats`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    pub quality: Quality,
    pub pictures_per_second: f64,
    pub sound_per_second: f64,
    pub megabits_per_second: f64,
    /// Online: the time there and back, the share of packets lost so far,
    /// and whether it goes through the relay.
    pub round_trip: Option<Duration>,
    pub lost: Option<f64>,
    pub relayed: Option<bool>,
}

/// A recording streamed to a viewer as it is made.
pub struct Stream {
    options: RecordOptions,
    video: VideoCodec,
    sound: SoundCodec,
    viewer: Viewer,
    quality: Quality,
    recorder: Option<Recorder>,
    gone: Arc<AtomicBool>,
    sent: Arc<Mutex<Sent>>,
    started: Instant,
    last: (Instant, u64, u64),
}

impl Stream {
    /// Stream what `options` records (its region set) to `viewer` at
    /// `quality`, with its pictures in the best codec from `best` down that
    /// both ends can, and its sound, if it has any, as `sound`.
    pub fn start(
        options: RecordOptions,
        viewer: Viewer,
        quality: Quality,
        best: VideoCodec,
        sound: SoundCodec,
    ) -> Result<Self> {
        let region = options.region.context("a stream needs its region")?;
        let video = playable(best, viewer.plays, live::encodes);
        let (width, height) = quality.size(region.width, region.height);
        {
            let mut out = viewer.pictures.lock().unwrap_or_else(|e| e.into_inner());
            LiveFormat {
                width,
                height,
                fps: options.fps,
                video,
                sound: options.sound_track.then_some(sound),
            }
            .write_to(&mut *out)?;
            out.flush()?;
        }
        let now = Instant::now();
        let mut stream = Self {
            options,
            video,
            sound,
            viewer,
            quality,
            recorder: None,
            gone: Arc::new(AtomicBool::new(false)),
            sent: Arc::new(Mutex::new(Sent::default())),
            started: now,
            last: (now, 0, 0),
        };
        stream.recorder = Some(stream.record()?);
        Ok(stream)
    }

    /// The codec the pictures go in.
    pub fn video(&self) -> VideoCodec {
        self.video
    }

    pub fn quality(&self) -> Quality {
        self.quality
    }

    /// Whether the viewer has gone: the stream has nowhere to go.
    pub fn gone(&self) -> bool {
        self.gone.load(Ordering::Relaxed)
    }

    /// The viewer.
    pub fn viewer(&self) -> &Viewer {
        &self.viewer
    }

    /// A recorder at the quality now. A new one's timeline starts again at
    /// zero: its frames are moved to now on the stream's, so the viewer's
    /// clock runs on.
    fn record(&self) -> Result<Recorder> {
        let region = self.options.region.context("a stream needs its region")?;
        let size = self.quality.size(region.width, region.height);
        let offset = (self.started.elapsed().as_nanos() / 100) as i64;
        Recorder::start_live(
            self.options.clone(),
            Live {
                sink: self.sink(offset),
                video: self.video,
                sound: self.sound,
                size: Some(size),
                bitrate: Some(self.quality.bitrate()),
            },
        )
    }

    /// Switch to `quality`: the recorder starts again at its size, and the
    /// viewer's player follows.
    pub fn set_quality(&mut self, quality: Quality) -> Result<()> {
        if quality == self.quality {
            return Ok(());
        }
        if let Some(recorder) = self.recorder.take() {
            recorder.stop()?;
        }
        self.quality = quality;
        self.recorder = Some(self.record()?);
        Ok(())
    }

    /// Switch a sound source on or off; see [`Recorder::set_sound`].
    pub fn set_sound(&self, source: Source, on: bool) {
        if let Some(recorder) = &self.recorder {
            recorder.set_sound(source, on);
        }
    }

    /// How the stream has gone since the last call.
    pub fn stats(&mut self) -> Stats {
        let (at, pictures, bytes) = self.last;
        let mut sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        let seconds = at.elapsed().as_secs_f64().max(0.001);
        let connection = self.viewer.connection.as_ref();
        let stats = Stats {
            quality: self.quality,
            pictures_per_second: (sent.pictures - pictures) as f64 / seconds,
            sound_per_second: sent.sound as f64 / seconds,
            megabits_per_second: (sent.bytes - bytes) as f64 * 8.0 / seconds / 1e6,
            round_trip: connection.map(Connection::round_trip),
            lost: connection.map(|c| {
                let (lost, sent) = c.lost();
                lost as f64 * 100.0 / sent.max(1) as f64
            }),
            relayed: connection.map(Connection::relayed),
        };
        sent.sound = 0;
        self.last = (Instant::now(), sent.pictures, sent.bytes);
        stats
    }

    /// How long it has streamed.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Stop recording: what the last recorder made.
    pub fn stop(mut self) -> Result<Option<RecordingSummary>> {
        self.recorder.take().map(Recorder::stop).transpose()
    }

    /// Each frame onto the viewer's pictures as it comes, `offset` later on
    /// its timeline, the sound onto its own if it has one, until the viewer
    /// is gone.
    fn sink(&self, offset: i64) -> LiveSink {
        let (out, sound_out, gone, sent) = (
            self.viewer.pictures.clone(),
            self.viewer.sound.clone(),
            self.gone.clone(),
            self.sent.clone(),
        );
        Arc::new(move |frame: LiveFrame| {
            if gone.load(Ordering::Relaxed) {
                return;
            }
            let frame = LiveFrame {
                time: frame.time + offset,
                ..frame
            };
            let written = {
                let out = match (frame.track, &sound_out) {
                    (Track::Sound, Some(sound_out)) => sound_out,
                    _ => &out,
                };
                let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
                frame.write_to(&mut *out).and_then(|()| Ok(out.flush()?))
            };
            if written.is_err() {
                gone.store(true, Ordering::Relaxed);
                return;
            }
            let mut sent = sent.lock().unwrap_or_else(|e| e.into_inner());
            sent.bytes += frame.data.len() as u64 + 22;
            match frame.track {
                Track::Video => sent.pictures += 1,
                Track::Sound => sent.sound += 1,
            }
        })
    }
}
