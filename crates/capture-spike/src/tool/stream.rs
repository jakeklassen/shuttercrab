//! Live streaming, spike S0 (issue #66): a recording sent over a local
//! socket as it is recorded, and a window that plays it as it arrives.
//! Both ends on one PC, to judge the picture, the sound and the delay
//! before any of it goes over a network.

use super::Args;
use anyhow::{Context, Result, ensure};
use capture_spike::{analysis::Roi, display};
use shuttercrab_capture::{
    MonitorId, PhysicalRect,
    live::{LiveFormat, LiveFrame, LivePlayer, SoundCodec, Track},
    record::{Live, LiveSink, RecordOptions, Recorder},
};
use std::{
    cell::RefCell,
    io::{BufReader, BufWriter, Write},
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
                Common::{DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
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

/// The recorder's log, on stderr: it says how late each source's sound
/// arrived, and how much came too late to mix.
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
    /// When the first picture reached the socket, less its time: the
    /// others are measured against it.
    first: Option<Duration>,
    /// How much later than the first any picture reached the socket, in
    /// the last second.
    worst: Duration,
}

pub(super) fn send(mut args: Args) -> Result<()> {
    let monitor = args.option("--monitor")?;
    let region = args
        .option("--region")?
        .map(|r| Roi::parse(&r))
        .transpose()?
        .unwrap_or(Roi {
            x: 0,
            y: 0,
            width: 2560,
            height: 1440,
        });
    let fps: u32 = args
        .option("--fps")?
        .map(|f| f.parse())
        .transpose()?
        .unwrap_or(60);
    let system_sound = args.option("--sound")?.as_deref() == Some("on");
    let sound_process = args
        .option("--sound-of")?
        .map(|p| p.parse::<u32>())
        .transpose()
        .context("--sound-of takes a process id")?;
    let microphone = args.option("--mic")?.as_deref() == Some("on");
    let codec = match args.option("--codec")?.as_deref() {
        None | Some("pcm") => SoundCodec::Pcm,
        Some("aac") => SoundCodec::Aac,
        Some(other) => anyhow::bail!("--codec takes pcm or aac, not {other}"),
    };
    let port: u16 = args
        .option("--port")?
        .map(|p| p.parse())
        .transpose()?
        .unwrap_or(DEFAULT_PORT);
    args.finish()?;
    let _ = log::set_logger(&Log);
    log::set_max_level(log::LevelFilter::Info);

    let monitors = display::enumerate()?;
    let target = display::select(&monitors, monitor.as_deref())?;
    let (width, height) = (region.width & !1, region.height & !1);
    let sound = system_sound || sound_process.is_some() || microphone;
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    println!(
        "{width}x{height} of {} at {fps} fps, {} ({codec:?}). Waiting for a viewer on \
         127.0.0.1:{port} (capture-spike stream-watch)...",
        target.device_name,
        describe_sound(sound_process, system_sound, microphone)
    );

    let (socket, viewer) = listener.accept()?;
    drop(listener);
    socket.set_nodelay(true)?;
    println!("Viewer {viewer} connected: streaming. Close the viewer, or Ctrl+C here, to stop.");

    let out = Arc::new(Mutex::new(BufWriter::new(socket)));
    LiveFormat {
        width,
        height,
        fps,
        sound: sound.then_some(codec),
    }
    .write_to(&mut *out.lock().unwrap())?;
    out.lock().unwrap().flush()?;
    let gone = Arc::new(AtomicBool::new(false));
    let sent = Arc::new(Mutex::new(Sent::default()));
    let started = Instant::now();
    let recorder = Recorder::start_live(
        RecordOptions {
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
        Live {
            sink: sink(out, gone.clone(), sent.clone(), started),
            sound: codec,
        },
    )?;

    report(&gone, &sent, started);
    let summary = recorder.stop()?;
    println!(
        "The viewer left. Streamed {:.0} s, {} pictures, {} encoder.",
        summary.duration.as_secs_f64(),
        summary.frames,
        if summary.hardware_encoder {
            "hardware"
        } else {
            "software"
        }
    );
    Ok(())
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

/// Each frame onto the socket as it comes, until the viewer is `gone`.
fn sink(
    out: Arc<Mutex<BufWriter<TcpStream>>>,
    gone: Arc<AtomicBool>,
    sent: Arc<Mutex<Sent>>,
    started: Instant,
) -> LiveSink {
    Arc::new(move |frame: LiveFrame| {
        if gone.load(Ordering::Relaxed) {
            return;
        }
        let written = {
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
            Track::Video => {
                sent.pictures += 1;
                // Against the first picture, which left about as soon as it
                // was taken.
                let late = started
                    .elapsed()
                    .saturating_sub(Duration::from_nanos(frame.time.max(0) as u64 * 100));
                let first = *sent.first.get_or_insert(late);
                sent.worst = sent.worst.max(late.saturating_sub(first));
            }
            Track::Sound => sent.sound += 1,
        }
    })
}

/// A status line a second until the viewer is `gone`.
fn report(gone: &AtomicBool, sent: &Mutex<Sent>, started: Instant) {
    let mut last = (0, 0);
    while !gone.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_secs(1));
        let mut sent = sent.lock().unwrap_or_else(|e| e.into_inner());
        println!(
            "{:>4} s: {} pictures/s, {} sound frames/s, {:.1} Mbit/s; latest picture {} ms behind the first",
            started.elapsed().as_secs(),
            sent.pictures - last.0,
            sent.sound,
            (sent.bytes - last.1) as f64 * 8.0 / 1e6,
            sent.worst.as_millis(),
        );
        last = (sent.pictures, sent.bytes);
        sent.sound = 0;
        sent.worst = Duration::ZERO;
    }
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
    let monitor = args.option("--monitor")?;
    let muted = args.option("--sound")?.as_deref() == Some("off");
    args.finish()?;

    let socket = TcpStream::connect(&from).with_context(|| format!("no stream at {from}"))?;
    socket.set_nodelay(true)?;
    let mut reader = BufReader::new(socket);
    let format = LiveFormat::read_from(&mut reader)?;
    println!(
        "{}x{} at {} fps, {}. Escape closes the window; M mutes; [ and ] move the \
         pictures earlier or later against the sound.",
        format.width,
        format.height,
        format.fps,
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
    let (reader_player, reader_bytes) = (player.clone(), bytes.clone());
    std::thread::spawn(move || {
        loop {
            match LiveFrame::read_from(&mut reader) {
                Ok(Some(frame)) => {
                    reader_bytes.fetch_add(frame.data.len() as u64 + 22, Ordering::Relaxed);
                    if let Err(e) = reader_player.push(frame) {
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
            "Shuttercrab live (spike) — {}{:.0} fps, {:.1} Mbit/s; behind the newest picture: \
             picture {} ms, {sound}; pictures lead by up to {} ms ([ ]){}",
            if self.ended { "ENDED — " } else { "" },
            (now - drawn) as f64 / seconds,
            (total - bytes) as f64 * 8.0 / seconds / 1e6,
            self.player.behind().as_millis(),
            self.player.lead().as_millis(),
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
