//! Live streaming, spike S0 (issue #66): a recording sent over a local
//! socket as it is recorded, and a window that plays it as it arrives.
//! Both ends on one PC, to judge the picture, the sound and the delay
//! before any of it goes over a network.

use super::Args;
use anyhow::{Context, Result, ensure};
use capture_spike::{analysis::Roi, display};
use shuttercrab_capture::{
    MonitorId, PhysicalRect,
    live::{LiveFormat, LiveFrame, LivePlayer, SoundCodec, Track, VideoCodec},
    record::{Live, LiveSink, RecordOptions, Recorder},
};
use std::{
    cell::RefCell,
    io::{BufReader, BufWriter, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::{
            Direct3D11::{ID3D11Device, ID3D11Texture2D},
            Dxgi::{
                Common::{
                    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN,
                    DXGI_SAMPLE_DESC,
                },
                DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
                DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice,
                IDXGIFactory2, IDXGISwapChain1,
            },
            Gdi::{GetMonitorInfoW, MONITORINFO},
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::AdjustWindowRectExForDpi,
            Input::KeyboardAndMouse::{VK_ESCAPE, VK_OEM_4, VK_OEM_6},
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, Interface, w},
};

const DEFAULT_PORT: u16 = 47_100;
/// The stream ended.
const WM_ENDED: u32 = WM_APP;

/// The recorder's and the player's log, on stderr: how late each source's
/// sound arrived and how much came too late to mix; when the played sound
/// ran dry or was cut.
struct Log;

impl log::Log for Log {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("{}: {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

/// What has gone over the socket, for the status lines.
#[derive(Default)]
struct Sent {
    pictures: u64,
    sound: u64,
    bytes: u64,
}

pub(super) fn send(mut args: Args) -> Result<()> {
    let monitor = args.option("--monitor")?;
    let region = args
        .option("--region")?
        .map(|r| Roi::parse(&r))
        .transpose()?;
    let fps: u32 = args
        .option("--fps")?
        .map(|f| f.parse())
        .transpose()?
        .unwrap_or(60);
    let quality = args
        .option("--quality")?
        .map(|q| Quality::parse(&q))
        .transpose()?
        .unwrap_or(Quality::P1440);
    let system_sound = args.option("--sound")?.as_deref() == Some("on");
    let sound_process = args
        .option("--sound-of")?
        .map(|p| p.parse::<u32>())
        .transpose()
        .context("--sound-of takes a process id")?;
    let microphone = args.option("--mic")?.as_deref() == Some("on");
    let (codec, video) = codecs(&mut args)?;
    let port: u16 = args
        .option("--port")?
        .map(|p| p.parse())
        .transpose()?
        .unwrap_or(DEFAULT_PORT);
    let online = args.option("--online")?.as_deref() == Some("on");
    let meet = Meet::from_args(&mut args)?;
    args.finish()?;
    let _ = log::set_logger(&Log);
    log::set_max_level(log::LevelFilter::Info);

    let monitors = display::enumerate()?;
    let target = display::select(&monitors, monitor.as_deref())?;
    // The whole monitor unless told otherwise.
    let region = region.unwrap_or(Roi {
        x: 0,
        y: 0,
        width: target.bounds.width,
        height: target.bounds.height,
    });
    let (width, height) = (region.width & !1, region.height & !1);
    let sound = system_sound || sound_process.is_some() || microphone;
    println!(
        "{width}x{height} of {} at {fps} fps, {} ({codec:?}).",
        target.device_name,
        describe_sound(sound_process, system_sound, microphone)
    );
    let (out, sound_out, connection, hevc) = open_output(online, &meet, port)?;
    let video = playable(video, hevc);
    let (w, h) = quality.size(width, height);
    LiveFormat {
        width: w,
        height: h,
        fps,
        video,
        sound: sound.then_some(codec),
    }
    .write_to(&mut *out.lock().unwrap())?;
    out.lock().unwrap().flush()?;

    let streaming = Streaming {
        options: RecordOptions {
            monitor: MonitorId::from_raw(target.hmonitor.0 as u64),
            region: Some(PhysicalRect::new(
                region.x as i32,
                region.y as i32,
                width,
                height,
            )),
            window: None,
            fps,
            include_cursor: true,
            system_sound: system_sound || sound_process.is_some(),
            sound_process,
            microphone,
            microphone_device: None,
            sound_track: sound,
            path: "unused.mp4".into(),
        },
        video,
        codec,
        out,
        sound_out,
        gone: Arc::new(AtomicBool::new(false)),
        sent: Arc::new(Mutex::new(Sent::default())),
        started: Instant::now(),
    };
    println!(
        "Streaming. 1, 2 and 3 switch to {}, {} and {}; close the viewer, or Ctrl+C here, to stop.",
        Quality::P1080.name(),
        Quality::P1440.name(),
        Quality::P2160.name()
    );
    streaming.run(quality, connection.as_ref())
}

/// A size to stream at, and the bit rate for it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quality {
    P1080,
    P1440,
    P2160,
}

impl Quality {
    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "1080p" => Self::P1080,
            "1440p" => Self::P1440,
            "4k" | "2160p" => Self::P2160,
            other => anyhow::bail!("--quality takes 1080p, 1440p or 4k, not {other}"),
        })
    }

    /// The quality a key in the terminal switches to.
    fn from_key(key: char) -> Option<Self> {
        match key {
            '1' => Some(Self::P1080),
            '2' => Some(Self::P1440),
            '3' => Some(Self::P2160),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::P1080 => "1080p",
            Self::P1440 => "1440p",
            Self::P2160 => "4K",
        }
    }

    /// Bits a second: under what Cloudflare's relay passes for one app
    /// (about 50 Mbit/s), with room for the peaks of busy pictures.
    fn bitrate(self) -> u32 {
        match self {
            Self::P1080 => 10_000_000,
            Self::P1440 => 18_000_000,
            Self::P2160 => 35_000_000,
        }
    }

    /// The size `w`×`h` is sent at: this height, the same shape, even, and
    /// never larger than it is.
    fn size(self, w: u32, h: u32) -> (u32, u32) {
        let target = match self {
            Self::P1080 => 1080,
            Self::P1440 => 1440,
            Self::P2160 => 2160,
        };
        if h <= target {
            return (w & !1, h & !1);
        }
        let scaled = (u64::from(w) * u64::from(target) / u64::from(h)) as u32;
        (scaled & !1, target)
    }
}

/// How the sound and the pictures are to go: `--codec` and `--video`.
fn codecs(args: &mut Args) -> Result<(SoundCodec, VideoCodec)> {
    let sound = match args.option("--codec")?.as_deref() {
        None | Some("pcm") => SoundCodec::Pcm,
        Some("aac") => SoundCodec::Aac,
        Some(other) => anyhow::bail!("--codec takes pcm or aac, not {other}"),
    };
    let video = match args.option("--video")?.as_deref() {
        None | Some("hevc") => VideoCodec::Hevc,
        Some("h264") => VideoCodec::H264,
        Some(other) => anyhow::bail!("--video takes hevc or h264, not {other}"),
    };
    Ok((sound, video))
}

/// `video`, unless it is HEVC and the viewer cannot play it (`hevc`):
/// then H.264.
fn playable(video: VideoCodec, hevc: bool) -> VideoCodec {
    let video = if video == VideoCodec::Hevc && !hevc {
        println!("The viewer cannot play HEVC (no HEVC Video Extensions): sending H.264.");
        VideoCodec::H264
    } else {
        video
    };
    println!("Pictures as {}.", describe_video(video));
    video
}

/// A stream under way: what to record, and where its frames go.
struct Streaming {
    options: RecordOptions,
    video: VideoCodec,
    codec: SoundCodec,
    out: SharedOut,
    sound_out: Option<SharedOut>,
    gone: Arc<AtomicBool>,
    sent: Arc<Mutex<Sent>>,
    started: Instant,
}

impl Streaming {
    /// A recorder at `quality`. A new one's timeline starts again at zero:
    /// its frames are moved to now on the stream's, so the viewer's clock
    /// runs on.
    fn record(&self, quality: Quality) -> Result<Recorder> {
        let region = self.options.region.expect("a region");
        let size = quality.size(region.width, region.height);
        let offset = (self.started.elapsed().as_nanos() / 100) as i64;
        Recorder::start_live(
            self.options.clone(),
            Live {
                sink: sink(self, offset),
                video: self.video,
                sound: self.codec,
                size: Some(size),
                bitrate: Some(quality.bitrate()),
            },
        )
    }

    /// Stream at `quality` until the viewer leaves, switching as keys in
    /// the terminal say, with a status line a second.
    fn run(
        &self,
        mut quality: Quality,
        connection: Option<&shuttercrab_connect::Connection>,
    ) -> Result<()> {
        let keys = keys();
        let mut recorder = self.record(quality)?;
        let mut last = (Instant::now(), 0, 0);
        while !self.gone.load(Ordering::Relaxed) {
            let wait = Duration::from_secs(1).saturating_sub(last.0.elapsed());
            if let Ok(key) = keys.recv_timeout(wait) {
                if let Some(chosen) = Quality::from_key(key).filter(|q| *q != quality) {
                    recorder.stop()?;
                    quality = chosen;
                    recorder = self.record(quality)?;
                    let region = self.options.region.expect("a region");
                    let (w, h) = quality.size(region.width, region.height);
                    println!(
                        "Now {} ({w}x{h}, {} Mbit/s).",
                        quality.name(),
                        quality.bitrate() / 1_000_000
                    );
                }
                continue;
            }
            last = report(&self.sent, self.started, last, quality, connection);
        }
        let summary = recorder.stop()?;
        println!(
            "The viewer left. Streamed {:.0} s, {} encoder.",
            self.started.elapsed().as_secs_f64(),
            if summary.hardware_encoder {
                "hardware"
            } else {
                "software"
            }
        );
        Ok(())
    }
}

/// Keys pressed in the terminal, as they are pressed.
fn keys() -> std::sync::mpsc::Receiver<char> {
    unsafe extern "C" {
        /// The C runtime's: the next key, without waiting for Enter.
        fn _getch() -> i32;
    }
    let (sender, keys) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        loop {
            // SAFETY: _getch takes nothing and returns a key code.
            let key = unsafe { _getch() };
            if key < 0 || sender.send(char::from(key as u8)).is_err() {
                break;
            }
        }
    });
    keys
}
/// Where the frames go.
type Out = Box<dyn Write + Send>;

/// The meeting point to stream or watch through, and how to connect:
/// `--meet URL` (Shuttercrab's unless given), and `--direct` to connect
/// without the relay.
struct Meet {
    url: String,
    path: shuttercrab_connect::Path,
}

impl Meet {
    fn from_args(args: &mut Args) -> Result<Self> {
        Ok(Self {
            url: args
                .option("--meet")?
                .unwrap_or_else(|| shuttercrab_connect::MEET.into()),
            path: if args.flag("--direct") {
                shuttercrab_connect::Path::Direct
            } else {
                shuttercrab_connect::Path::Relay
            },
        })
    }
}

/// How a connection goes, for the status lines.
fn how(connection: &shuttercrab_connect::Connection) -> &'static str {
    if connection.relayed() {
        "through the relay"
    } else {
        "directly"
    }
}

/// The first byte of each stream online, so the viewer can tell them
/// apart: the format and the pictures, and the sound.
const PICTURES: u8 = b'P';
const SOUND: u8 = b'S';

/// What the viewer says first: whether it can play HEVC.
const PLAYS_HEVC: u8 = b'H';
const PLAYS_H264: u8 = b'h';

/// What this PC can play, for the sender.
fn plays() -> u8 {
    if LivePlayer::decodes(VideoCodec::Hevc) {
        PLAYS_HEVC
    } else {
        PLAYS_H264
    }
}

fn describe_video(video: VideoCodec) -> &'static str {
    match video {
        VideoCodec::H264 => "H.264, 8 bits",
        VideoCodec::Hevc => "HEVC, 10 bits",
    }
}

/// Where the frames go, for the threads that send them.
type SharedOut = Arc<Mutex<BufWriter<Out>>>;

fn shared(out: Out) -> SharedOut {
    Arc::new(Mutex::new(BufWriter::new(out)))
}

/// A viewer anywhere if `online`, through `meet`, else on this PC: where
/// the pictures go, the sound if it goes apart from them, and whether the
/// viewer can play HEVC.
fn open_output(
    online: bool,
    meet: &Meet,
    port: u16,
) -> Result<(
    SharedOut,
    Option<SharedOut>,
    Option<shuttercrab_connect::Connection>,
    bool,
)> {
    if online {
        let (pictures, sound, connection, hevc) = host_online(meet)?;
        Ok((
            shared(pictures),
            Some(shared(sound)),
            Some(connection),
            hevc,
        ))
    } else {
        let (out, hevc) = wait_locally(port)?;
        Ok((shared(out), None, None, hevc))
    }
}

/// A viewer on this PC, from stream-watch without a link, and whether it
/// can play HEVC.
fn wait_locally(port: u16) -> Result<(Out, bool)> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    println!("Waiting for a viewer on 127.0.0.1:{port} (capture-spike stream-watch)...");
    let (socket, viewer) = listener.accept()?;
    socket.set_nodelay(true)?;
    println!("Viewer {viewer} connected.");
    let mut plays = [0];
    (&socket).read_exact(&mut plays)?;
    Ok((Box::new(socket), plays[0] == PLAYS_HEVC))
}

/// A viewer anywhere: a room at `meet`, a link to share, and a direct
/// connection to whoever comes with it. The sound has a stream of its own,
/// sent first, so a large picture cannot hold it up. Whether the viewer can
/// play HEVC comes on a stream from it.
fn host_online(meet: &Meet) -> Result<(Out, Out, shuttercrab_connect::Connection, bool)> {
    let hosting = shuttercrab_connect::host(&meet.url, &host_key()?, meet.path)?;
    println!(
        "Share this link; whoever has it can watch:\n\n  {}\n\n\
         Waiting for a viewer (capture-spike stream-watch --link LINK)...",
        hosting.link()
    );
    let connection = hosting.accept()?;
    println!(
        "Viewer at {} connected {}, {} ms there and back.",
        connection.remote(),
        how(&connection),
        connection.round_trip().as_millis()
    );
    let mut plays = [0];
    connection.receive()?.read_exact(&mut plays)?;
    let mut pictures = connection.send(0)?;
    pictures.write_all(&[PICTURES])?;
    let mut sound = connection.send(1)?;
    sound.write_all(&[SOUND])?;
    Ok((
        Box::new(pictures),
        Box::new(sound),
        connection,
        plays[0] == PLAYS_HEVC,
    ))
}

/// The key that opens a room on the meeting point: `SHUTTERCRAB_HOST_KEY`,
/// or `meet/.env` when run from the repository. Never printed.
fn host_key() -> Result<String> {
    if let Ok(key) = std::env::var("SHUTTERCRAB_HOST_KEY") {
        return Ok(key);
    }
    std::fs::read_to_string("meet/.env")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("HOST_KEY="))
                .map(|key| key.trim().to_string())
        })
        .context("no host key: set SHUTTERCRAB_HOST_KEY, or run from the repository, whose meet/.env holds it")
}

/// What the stream's sound is, for the first line.
fn describe_sound(process: Option<u32>, speakers: bool, microphone: bool) -> String {
    match (process, speakers, microphone) {
        (Some(pid), _, true) => format!("process {pid}'s sound and the microphone"),
        (Some(pid), _, false) => format!("process {pid}'s sound"),
        (None, true, true) => "the speakers' sound and the microphone".into(),
        (None, true, false) => "the speakers' sound".into(),
        (None, false, true) => "the microphone".into(),
        (None, false, false) => "no sound".into(),
    }
}

/// Each frame onto the stream's socket as it comes, `offset` later on its
/// timeline, the sound onto its own if it has one, until the viewer is
/// gone.
fn sink(streaming: &Streaming, offset: i64) -> LiveSink {
    let (out, sound_out, gone, sent) = (
        streaming.out.clone(),
        streaming.sound_out.clone(),
        streaming.gone.clone(),
        streaming.sent.clone(),
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

/// A status line for the time since `last` (when, pictures and bytes
/// sent by then), with how the `connection` is doing if it is online: the
/// new `last`.
fn report(
    sent: &Mutex<Sent>,
    started: Instant,
    (at, pictures, bytes): (Instant, u64, u64),
    quality: Quality,
    connection: Option<&shuttercrab_connect::Connection>,
) -> (Instant, u64, u64) {
    let mut sent = sent.lock().unwrap_or_else(|e| e.into_inner());
    let seconds = at.elapsed().as_secs_f64().max(0.001);
    println!(
        "{:>4} s: {}, {:.0} pictures/s, {:.0} sound frames/s, {:.1} Mbit/s{}",
        started.elapsed().as_secs(),
        quality.name(),
        (sent.pictures - pictures) as f64 / seconds,
        sent.sound as f64 / seconds,
        (sent.bytes - bytes) as f64 * 8.0 / seconds / 1e6,
        connection.map(network).unwrap_or_default(),
    );
    sent.sound = 0;
    (Instant::now(), sent.pictures, sent.bytes)
}

/// How an online connection is doing, for the status lines.
fn network(connection: &shuttercrab_connect::Connection) -> String {
    let (lost, sent) = connection.lost();
    format!(
        "; {} ms there and back, {:.2}% of packets lost",
        connection.round_trip().as_millis(),
        lost as f64 * 100.0 / sent.max(1) as f64
    )
}

/// The viewer window's state, on its thread. The pictures are drawn on a
/// thread of their own, so the window keeps up while they are.
struct Viewer {
    player: Arc<LivePlayer>,
    bytes: Arc<AtomicU64>,
    /// Pictures drawn, counted by the drawing thread.
    drawn: Arc<AtomicU64>,
    /// Tells the drawing thread to stop.
    stop: Arc<AtomicBool>,
    last: (Instant, u64, u64),
    muted: bool,
    ended: bool,
    /// The connection to the stream, if it is online.
    connection: Option<shuttercrab_connect::Connection>,
}

thread_local! {
    static VIEWER: RefCell<Option<Viewer>> = const { RefCell::new(None) };
}

/// How much [ and ] change how far ahead of the sound pictures are shown.
const LEAD_STEP: Duration = Duration::from_millis(20);

pub(super) fn watch(mut args: Args) -> Result<()> {
    let from = args
        .option("--from")?
        .unwrap_or_else(|| format!("127.0.0.1:{DEFAULT_PORT}"));
    let link = args.option("--link")?;
    let meet = Meet::from_args(&mut args)?;
    let monitor = args.option("--monitor")?;
    let muted = args.option("--sound")?.as_deref() == Some("off");
    args.finish()?;
    let _ = log::set_logger(&Log);
    log::set_max_level(log::LevelFilter::Info);

    let (input, sound_input, connection) = open_input(link.as_deref(), &meet, &from)?;
    let mut reader = BufReader::new(input);
    let format = LiveFormat::read_from(&mut reader)?;
    println!(
        "{}x{} at {} fps, {}, {}. Escape closes the window; M mutes; [ and ] move the \
         pictures earlier or later against the sound.",
        format.width,
        format.height,
        format.fps,
        describe_video(format.video),
        match format.sound {
            Some(SoundCodec::Aac) => "AAC sound",
            Some(SoundCodec::Pcm) => "PCM sound",
            None => "no sound",
        }
    );

    let monitors = display::enumerate()?;
    let target = display::select(&monitors, monitor.as_deref())?;
    let hwnd = open_window(&format, target)?;
    let player = Arc::new(LivePlayer::new(format, |why| {
        eprintln!("the sound stopped: {why}");
    })?);
    player.set_muted(muted);

    let window = hwnd.0 as usize;
    let bytes = Arc::new(AtomicU64::new(0));
    if let Some(sound_input) = sound_input {
        let (player, bytes) = (player.clone(), bytes.clone());
        std::thread::spawn(move || read_frames(BufReader::new(sound_input), &player, &bytes));
    }
    let (reader_player, reader_bytes) = (player.clone(), bytes.clone());
    std::thread::spawn(move || {
        read_frames(reader, &reader_player, &reader_bytes);
        let _ =
            unsafe { PostMessageW(Some(HWND(window as *mut _)), WM_ENDED, WPARAM(0), LPARAM(0)) };
    });

    let (drawn, stop) = (
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicBool::new(false)),
    );
    let drawing = {
        let (player, drawn, stop) = (player.clone(), drawn.clone(), stop.clone());
        std::thread::spawn(move || {
            let hwnd = HWND(window as *mut _);
            if let Err(e) = draw(&player, hwnd, (format.width, format.height), &drawn, &stop)
                && !stop.load(Ordering::Relaxed)
            {
                eprintln!("drawing stopped: {e:#}");
            }
        })
    };

    VIEWER.with_borrow_mut(|viewer| {
        *viewer = Some(Viewer {
            player,
            bytes,
            drawn,
            stop: stop.clone(),
            last: (Instant::now(), 0, 0),
            muted,
            ended: false,
            connection,
        })
    });
    unsafe {
        SetTimer(Some(hwnd), 1, 500, None);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    stop.store(true, Ordering::Relaxed);
    let _ = drawing.join();
    VIEWER.with_borrow_mut(|viewer| viewer.take());
    Ok(())
}

/// Hand each frame from `reader` to `player` until the stream ends.
fn read_frames(mut reader: impl Read, player: &LivePlayer, bytes: &AtomicU64) {
    loop {
        match LiveFrame::read_from(&mut reader) {
            Ok(Some(frame)) => {
                bytes.fetch_add(frame.data.len() as u64 + 22, Ordering::Relaxed);
                if let Err(e) = player.push(frame) {
                    eprintln!("{e:#}");
                }
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("the stream broke: {e:#}");
                break;
            }
        }
    }
}

/// Where frames come from.
type In = Box<dyn Read + Send>;

/// The stream `link` is for, through `meet`, or else the one on this PC
/// at `from`: the format and pictures, and the sound if it comes apart
/// from them.
fn open_input(
    link: Option<&str>,
    meet: &Meet,
    from: &str,
) -> Result<(In, Option<In>, Option<shuttercrab_connect::Connection>)> {
    let Some(link) = link else {
        let socket = TcpStream::connect(from).with_context(|| format!("no stream at {from}"))?;
        socket.set_nodelay(true)?;
        (&socket).write_all(&[plays()])?;
        return Ok((Box::new(socket), None, None));
    };
    let link = shuttercrab_connect::Link::parse(link)?;
    println!("Joining the stream...");
    let connection = shuttercrab_connect::join(&meet.url, &link, meet.path)?;
    println!(
        "Connected {} to {}, {} ms there and back.",
        how(&connection),
        connection.remote(),
        connection.round_trip().as_millis()
    );
    connection.send(0)?.write_all(&[plays()])?;
    // Two streams, each tagged; they may arrive in either order.
    let (mut pictures, mut sound): (In, In) = (
        Box::new(connection.receive()?),
        Box::new(connection.receive()?),
    );
    let mut tag = [0];
    pictures.read_exact(&mut tag)?;
    if tag[0] == SOUND {
        std::mem::swap(&mut pictures, &mut sound);
        pictures.read_exact(&mut tag)?;
        ensure!(tag[0] == PICTURES, "the stream's pictures are missing");
    } else {
        ensure!(tag[0] == PICTURES, "not a Shuttercrab stream");
        sound.read_exact(&mut tag)?;
        ensure!(tag[0] == SOUND, "the stream's sound is missing");
    }
    Ok((pictures, Some(sound), Some(connection)))
}

/// Draw each picture as it falls due, once a refresh of the display the
/// window is on, until `stop`.
fn draw(
    player: &LivePlayer,
    hwnd: HWND,
    (width, height): (u32, u32),
    drawn: &AtomicU64,
    stop: &AtomicBool,
) -> Result<()> {
    let swap_chain = swap_chain(&player.device(), hwnd, width, height)?;
    let mut size = (width, height);
    let mut output = None;
    let mut refreshes = 0u32;
    while !stop.load(Ordering::Relaxed) {
        // The window may have moved to another display: look again now
        // and then.
        if refreshes.is_multiple_of(120) {
            output = unsafe { swap_chain.GetContainingOutput() }.ok();
        }
        refreshes = refreshes.wrapping_add(1);
        match &output {
            Some(output) => unsafe { output.WaitForVBlank() }?,
            None => std::thread::sleep(Duration::from_millis(8)),
        }
        // The sender changed its quality: the buffers follow the pictures,
        // and the window stretches them as before.
        let now = player.size();
        if now != size {
            unsafe {
                swap_chain.ResizeBuffers(0, now.0, now.1, DXGI_FORMAT_UNKNOWN, Default::default())
            }?;
            size = now;
        }
        let back: ID3D11Texture2D = unsafe { swap_chain.GetBuffer(0) }?;
        if player.draw(&back)? {
            // Just after a refresh: shown at the next.
            unsafe { swap_chain.Present(0, DXGI_PRESENT(0)) }.ok()?;
            drawn.fetch_add(1, Ordering::Relaxed);
        }
    }
    Ok(())
}

/// A window for `format`'s pictures, at their size where it fits on
/// `monitor` and scaled down where it does not, in the middle.
fn open_window(format: &LiveFormat, monitor: &display::Monitor) -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: w!("ShuttercrabLiveSpike"),
            ..Default::default()
        };
        ensure!(
            RegisterClassW(&class) != 0,
            "could not register the window class"
        );
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(monitor.hmonitor, &mut info);
        let work = info.rcWork;
        let (room_w, room_h) = (
            (work.right - work.left) as f64 * 0.95,
            (work.bottom - work.top) as f64 * 0.9,
        );
        let scale = (room_w / format.width as f64)
            .min(room_h / format.height as f64)
            .min(1.0);
        let (w, h) = (
            (format.width as f64 * scale) as i32,
            (format.height as f64 * scale) as i32,
        );
        let style = WS_OVERLAPPEDWINDOW;
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        AdjustWindowRectExForDpi(&mut frame, style, false, WINDOW_EX_STYLE(0), monitor.dpi)?;
        let (fw, fh) = (frame.right - frame.left, frame.bottom - frame.top);
        let x = work.left + ((work.right - work.left) - fw) / 2;
        let y = work.top + ((work.bottom - work.top) - fh) / 2;
        let title = HSTRING::from(format!(
            "Shuttercrab live (spike) — {}x{}{}",
            format.width,
            format.height,
            if scale < 1.0 {
                format!(", shown at {:.0}%", scale * 100.0)
            } else {
                String::new()
            }
        ));
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("ShuttercrabLiveSpike"),
            &title,
            style,
            x,
            y,
            fw,
            fh,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        Ok(hwnd)
    }
}

/// A swap chain at the pictures' own size; the window stretches it.
fn swap_chain(
    device: &ID3D11Device,
    hwnd: HWND,
    width: u32,
    height: u32,
) -> Result<IDXGISwapChain1> {
    unsafe {
        let factory: IDXGIFactory2 = device.cast::<IDXGIDevice>()?.GetAdapter()?.GetParent()?;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width,
            Height: height,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            ..Default::default()
        };
        factory
            .CreateSwapChainForHwnd(device, hwnd, &desc, None, None)
            .context("could not make a swap chain")
    }
}

impl Viewer {
    /// The window title with the latest numbers.
    fn status(&mut self) -> String {
        let (at, drawn, bytes) = self.last;
        let seconds = at.elapsed().as_secs_f64().max(0.001);
        let (now, total) = (
            self.drawn.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
        );
        let sound = match self.player.sound_behind() {
            Some(behind) => format!("sound {} ms", behind.as_millis()),
            None => "no sound".into(),
        };
        let title = format!(
            "Shuttercrab live (spike) — {}{}x{}, {:.0} fps, {:.1} Mbit/s; behind the newest \
             picture: picture {} ms, {sound}; pictures lead by up to {} ms ([ ]){}{}",
            if self.ended { "ENDED — " } else { "" },
            self.player.size().0,
            self.player.size().1,
            (now - drawn) as f64 / seconds,
            (total - bytes) as f64 * 8.0 / seconds / 1e6,
            self.player.behind().as_millis(),
            self.player.lead().as_millis(),
            self.connection.as_ref().map(network).unwrap_or_default(),
            if self.muted { ", muted (M)" } else { "" }
        );
        self.last = (Instant::now(), now, total);
        title
    }

    /// Show pictures `step` earlier (or later, if negative) against the
    /// sound.
    fn move_lead(&mut self, earlier: bool) -> String {
        let lead = self.player.lead();
        self.player.set_lead(if earlier {
            lead + LEAD_STEP
        } else {
            lead.saturating_sub(LEAD_STEP)
        });
        self.status()
    }
}

/// What the window does once the viewer is no longer borrowed: these send
/// messages straight back to `window_proc`.
enum After {
    Title(String),
    Close,
}

extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let handled = VIEWER.with_borrow_mut(|viewer| {
        let viewer = viewer.as_mut()?;
        Some(match message {
            WM_TIMER => After::Title(viewer.status()),
            WM_ENDED => {
                viewer.ended = true;
                After::Title(viewer.status())
            }
            WM_KEYDOWN if wparam.0 == usize::from(VK_ESCAPE.0) => After::Close,
            WM_KEYDOWN if wparam.0 == usize::from(b'M') => {
                viewer.muted = !viewer.muted;
                viewer.player.set_muted(viewer.muted);
                After::Title(viewer.status())
            }
            WM_KEYDOWN if wparam.0 == usize::from(VK_OEM_6.0) => {
                After::Title(viewer.move_lead(true))
            }
            WM_KEYDOWN if wparam.0 == usize::from(VK_OEM_4.0) => {
                After::Title(viewer.move_lead(false))
            }
            WM_DESTROY => {
                // Before the window goes: drawing into it fails after.
                viewer.stop.store(true, Ordering::Relaxed);
                return None;
            }
            _ => return None,
        })
    });
    if let Some(after) = handled {
        match after {
            After::Title(title) => {
                let _ = unsafe { SetWindowTextW(hwnd, &HSTRING::from(title)) };
            }
            After::Close => {
                let _ = unsafe { DestroyWindow(hwnd) };
            }
        }
        return LRESULT(0);
    }
    if message == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}
