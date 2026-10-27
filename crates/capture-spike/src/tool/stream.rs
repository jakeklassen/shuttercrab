//! Live streaming, spike S0 (issue #66): a recording sent over a local
//! socket as it is recorded, and a window that plays it as it arrives.
//! Both ends on one PC, to judge the picture, the sound and the delay
//! before any of it goes over a network.

use super::Args;
use anyhow::{Context, Result, ensure};
use capture_spike::{analysis::Roi, display, gpu::Gpu};
use shuttercrab_capture::{
    MonitorId, PhysicalRect,
    live::{LiveFormat, LiveFrame, LivePlayer, Track},
    record::{LiveSink, RecordOptions, Recorder},
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
    Graphics::DirectX::Direct3D11::IDirect3DSurface,
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::{
            Dxgi::{
                Common::{DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
                DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice,
                IDXGIFactory2, IDXGISurface, IDXGISwapChain1,
            },
            Gdi::{GetMonitorInfoW, MONITORINFO},
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            WinRT::Direct3D11::CreateDirect3D11SurfaceFromDXGISurface,
        },
        UI::{
            HiDpi::AdjustWindowRectExForDpi, Input::KeyboardAndMouse::VK_ESCAPE,
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, Interface, w},
};

const DEFAULT_PORT: u16 = 47_100;
/// A new picture is ready to show.
const WM_FRAME: u32 = WM_APP;
/// The stream ended.
const WM_ENDED: u32 = WM_APP + 1;

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
    let port: u16 = args
        .option("--port")?
        .map(|p| p.parse())
        .transpose()?
        .unwrap_or(DEFAULT_PORT);
    args.finish()?;

    let monitors = display::enumerate()?;
    let target = display::select(&monitors, monitor.as_deref())?;
    let (width, height) = (region.width & !1, region.height & !1);
    let sound = system_sound || sound_process.is_some() || microphone;
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    println!(
        "{width}x{height} of {} at {fps} fps, {}. Waiting for a viewer on 127.0.0.1:{port} \
         (capture-spike stream-watch)...",
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
        sound,
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
        sink(out, gone.clone(), sent.clone(), started),
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

/// The viewer window's state, on its thread.
struct Viewer {
    player: Arc<LivePlayer>,
    swap_chain: IDXGISwapChain1,
    /// A picture is waiting to be shown: no more messages until it is.
    pending: Arc<AtomicBool>,
    bytes: Arc<AtomicU64>,
    shown: u64,
    last: (Instant, u64, u64),
    muted: bool,
    ended: bool,
    _gpu: Gpu,
}

thread_local! {
    static VIEWER: RefCell<Option<Viewer>> = const { RefCell::new(None) };
}

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
        "{}x{} at {} fps, {}. Escape closes the window; M mutes.",
        format.width,
        format.height,
        format.fps,
        if format.sound {
            "with sound"
        } else {
            "no sound"
        }
    );

    let monitors = display::enumerate()?;
    let target = display::select(&monitors, monitor.as_deref())?;
    let hwnd = open_window(&format, target)?;
    let gpu = Gpu::video(None)?;
    let swap_chain = swap_chain(&gpu, hwnd, &format)?;

    let pending = Arc::new(AtomicBool::new(false));
    let frame_pending = pending.clone();
    let window = hwnd.0 as usize;
    let player = Arc::new(LivePlayer::new(
        format,
        |why| eprintln!("the player stopped: {why}"),
        move || {
            if !frame_pending.swap(true, Ordering::AcqRel) {
                let _ = unsafe {
                    PostMessageW(Some(HWND(window as *mut _)), WM_FRAME, WPARAM(0), LPARAM(0))
                };
            }
        },
    )?);
    player.set_muted(muted)?;

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

    VIEWER.with_borrow_mut(|viewer| {
        *viewer = Some(Viewer {
            player,
            swap_chain,
            pending,
            bytes,
            shown: 0,
            last: (Instant::now(), 0, 0),
            muted,
            ended: false,
            _gpu: gpu,
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
    VIEWER.with_borrow_mut(|viewer| viewer.take());
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
fn swap_chain(gpu: &Gpu, hwnd: HWND, format: &LiveFormat) -> Result<IDXGISwapChain1> {
    unsafe {
        let factory: IDXGIFactory2 = gpu
            .device
            .cast::<IDXGIDevice>()?
            .GetAdapter()?
            .GetParent()?;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: format.width,
            Height: format.height,
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
            .CreateSwapChainForHwnd(&gpu.device, hwnd, &desc, None, None)
            .context("could not make a swap chain")
    }
}

impl Viewer {
    /// Copy the newest picture into the swap chain and show it.
    fn show(&mut self) -> Result<()> {
        self.pending.store(false, Ordering::Release);
        unsafe {
            let back: IDXGISurface = self.swap_chain.GetBuffer(0)?;
            let surface: IDirect3DSurface =
                CreateDirect3D11SurfaceFromDXGISurface(&back)?.cast()?;
            self.player.copy_frame(&surface)?;
            self.swap_chain.Present(1, DXGI_PRESENT(0)).ok()?;
        }
        self.shown += 1;
        Ok(())
    }

    fn status(&mut self, hwnd: HWND) {
        let (at, shown, bytes) = self.last;
        let seconds = at.elapsed().as_secs_f64().max(0.001);
        let total = self.bytes.load(Ordering::Relaxed);
        let title = format!(
            "Shuttercrab live (spike) — {}{:.0} fps shown, {:.1} Mbit/s, {} ms behind the newest picture{}",
            if self.ended { "ENDED — " } else { "" },
            (self.shown - shown) as f64 / seconds,
            (total - bytes) as f64 * 8.0 / seconds / 1e6,
            self.player.behind().as_millis(),
            if self.muted { ", muted (M)" } else { "" }
        );
        self.last = (Instant::now(), self.shown, total);
        let _ = unsafe { SetWindowTextW(hwnd, &HSTRING::from(title)) };
    }
}

extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let handled = VIEWER.with_borrow_mut(|viewer| {
        let viewer = viewer.as_mut()?;
        match message {
            WM_FRAME => {
                if let Err(e) = viewer.show() {
                    eprintln!("{e:#}");
                }
            }
            WM_TIMER => viewer.status(hwnd),
            WM_ENDED => {
                viewer.ended = true;
                viewer.status(hwnd);
            }
            WM_KEYDOWN if wparam.0 == usize::from(VK_ESCAPE.0) => {
                let _ = unsafe { DestroyWindow(hwnd) };
            }
            WM_KEYDOWN if wparam.0 == usize::from(b'M') => {
                viewer.muted = !viewer.muted;
                let _ = viewer.player.set_muted(viewer.muted);
                viewer.status(hwnd);
            }
            _ => return None,
        }
        Some(LRESULT(0))
    });
    if let Some(result) = handled {
        return result;
    }
    if message == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}
