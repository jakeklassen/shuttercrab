//! The watching side: a stream opened, and the window that plays it.

use crate::{PICTURES, Path, SOUND, describe};
use anyhow::{Context, Result, ensure};
use shuttercrab_capture::live::{LiveFormat, LiveFrame, LivePlayer, SoundCodec, VideoCodec};
use shuttercrab_connect::{Connection, Link};
use std::{
    cell::RefCell,
    io::{BufReader, Read, Write},
    net::{Shutdown, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
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
            Gdi::{
                GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
            },
        },
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx},
            LibraryLoader::GetModuleHandleW,
        },
        UI::{
            HiDpi::{
                AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                GetDpiForMonitor, MDT_EFFECTIVE_DPI, SetProcessDpiAwarenessContext,
            },
            Input::KeyboardAndMouse::{VK_ESCAPE, VK_OEM_4, VK_OEM_6},
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, Interface, PCWSTR, w},
};

/// The stream ended.
const WM_ENDED: u32 = WM_APP;

/// How much [ and ] change how far ahead of the sound pictures are shown.
const LEAD_STEP: Duration = Duration::from_millis(20);

/// Where frames come from.
type In = Box<dyn Read + Send>;

/// A stream to watch.
pub struct Input {
    /// The format and pictures.
    pictures: In,
    /// The sound, if it comes apart from the pictures.
    sound: Option<In>,
    /// The connection, if the stream is online.
    connection: Option<Connection>,
    /// The socket, if it is on this PC: shut down to stop reading.
    socket: Option<TcpStream>,
}

/// What the viewer says first: the codecs it can play, a bit each.
fn plays() -> u8 {
    VideoCodec::BEST_FIRST
        .into_iter()
        .filter(|&codec| LivePlayer::decodes(codec))
        .fold(0, |set, codec| set | codec.bit())
}

impl Input {
    /// The stream on this PC at `from` (`127.0.0.1:port`).
    pub fn local(from: &str) -> Result<Self> {
        let socket = TcpStream::connect(from).with_context(|| format!("no stream at {from}"))?;
        socket.set_nodelay(true)?;
        (&socket).write_all(&[plays()])?;
        Ok(Self {
            pictures: Box::new(socket.try_clone()?),
            sound: None,
            connection: None,
            socket: Some(socket),
        })
    }

    /// The stream `link` is for, through `meet`, reached by `path`.
    pub fn online(link: &Link, meet: &str, path: Path) -> Result<Self> {
        let connection = shuttercrab_connect::join(meet, link, path)?;
        log::info!(
            "connected {} to {}, {} ms there and back",
            if connection.relayed() {
                "through the relay"
            } else {
                "directly"
            },
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
        Ok(Self {
            pictures,
            sound: Some(sound),
            connection: Some(connection),
            socket: None,
        })
    }
}

/// How to show a stream.
#[derive(Clone, Debug, Default)]
pub struct WatchOptions {
    /// The monitor to open on: the one under the pointer if none.
    pub monitor: Option<HMONITOR>,
    pub muted: bool,
    /// What the window's title starts with.
    pub title: String,
}

/// The window's state, on its thread. The pictures are drawn on a thread of
/// their own, so the window keeps up while they are.
struct Window {
    player: Arc<LivePlayer>,
    bytes: Arc<AtomicU64>,
    /// Pictures drawn, counted by the drawing thread.
    drawn: Arc<AtomicU64>,
    /// Tells the drawing thread to stop.
    stop: Arc<AtomicBool>,
    last: (Instant, u64, u64),
    muted: bool,
    ended: bool,
    title: String,
    /// The connection to the stream, if it is online.
    connection: Option<Connection>,
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// Play `input` in a window until it is closed: Escape closes it, M mutes,
/// [ and ] show the pictures 20 ms earlier or later against the sound.
pub fn watch(input: Input, options: &WatchOptions) -> Result<()> {
    // Physical pixels, as the app has, and COM for the sound and the
    // decoders. Either may be set up already.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let Input {
        pictures,
        sound: sound_input,
        connection,
        socket,
    } = input;
    let mut reader = BufReader::new(pictures);
    let format = LiveFormat::read_from(&mut reader)?;
    log::info!(
        "{}x{} at {} fps, {}, {}",
        format.width,
        format.height,
        format.fps,
        describe(format.video),
        match format.sound {
            Some(SoundCodec::Aac) => "AAC sound",
            Some(SoundCodec::Pcm) => "PCM sound",
            Some(SoundCodec::Opus) => "Opus sound",
            None => "no sound",
        }
    );

    let monitor = options.monitor.unwrap_or_else(pointer_monitor);
    let hwnd = open_window(&format, monitor, &options.title)?;
    let player = Arc::new(LivePlayer::new(format, |why| {
        log::warn!("the sound stopped: {why}");
    })?);
    player.set_muted(options.muted);

    let window = hwnd.0 as usize;
    let bytes = Arc::new(AtomicU64::new(0));
    let mut readers = Vec::new();
    if let Some(sound_input) = sound_input {
        let (player, bytes) = (player.clone(), bytes.clone());
        readers.push(std::thread::spawn(move || {
            read_frames(BufReader::new(sound_input), &player, &bytes);
        }));
    }
    let (reader_player, reader_bytes) = (player.clone(), bytes.clone());
    readers.push(std::thread::spawn(move || {
        read_frames(reader, &reader_player, &reader_bytes);
        let _ =
            unsafe { PostMessageW(Some(HWND(window as *mut _)), WM_ENDED, WPARAM(0), LPARAM(0)) };
    }));

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
                log::error!("drawing stopped: {e:#}");
            }
        })
    };

    WINDOW.with_borrow_mut(|state| {
        *state = Some(Window {
            player,
            bytes,
            drawn,
            stop: stop.clone(),
            last: (Instant::now(), 0, 0),
            muted: options.muted,
            ended: false,
            title: options.title.clone(),
            connection,
        });
    });
    unsafe {
        SetTimer(Some(hwnd), 1, 500, None);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    stop.store(true, Ordering::Relaxed);
    let _ = drawing.join();
    // Close the connection, which ends the readers, and wait for them, so
    // the player and its sound are let go here, before the process ends,
    // rather than mid-call on another thread as it does.
    WINDOW.with_borrow_mut(Option::take);
    if let Some(socket) = socket {
        let _ = socket.shutdown(Shutdown::Both);
    }
    for reader in readers {
        let _ = reader.join();
    }
    Ok(())
}

/// Tell the user `text` in a message box titled `title`, for a viewer with
/// no window yet to say it in.
pub fn tell(title: &str, text: &str) {
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from(title),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

/// The monitor under the pointer.
fn pointer_monitor() -> HMONITOR {
    let mut at = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut at);
        MonitorFromPoint(at, MONITOR_DEFAULTTOPRIMARY)
    }
}

/// Hand each frame from `reader` to `player` until the stream ends.
fn read_frames(mut reader: impl Read, player: &LivePlayer, bytes: &AtomicU64) {
    loop {
        match LiveFrame::read_from(&mut reader) {
            Ok(Some(frame)) => {
                bytes.fetch_add(frame.data.len() as u64 + 22, Ordering::Relaxed);
                if let Err(e) = player.push(frame) {
                    log::warn!("{e:#}");
                }
            }
            Ok(None) => break,
            Err(e) => {
                log::warn!("the stream broke: {e:#}");
                break;
            }
        }
    }
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
fn open_window(format: &LiveFormat, monitor: HMONITOR, title: &str) -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            // The program's icon, resource 1, if it has one.
            hIcon: LoadIconW(Some(instance.into()), PCWSTR(1 as _)).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: w!("ShuttercrabLive"),
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
        let _ = GetMonitorInfoW(monitor, &mut info);
        let (mut dpi, mut unused) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut unused);
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
        AdjustWindowRectExForDpi(&mut frame, style, false, WINDOW_EX_STYLE(0), dpi)?;
        let (fw, fh) = (frame.right - frame.left, frame.bottom - frame.top);
        let x = work.left + ((work.right - work.left) - fw) / 2;
        let y = work.top + ((work.bottom - work.top) - fh) / 2;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("ShuttercrabLive"),
            &HSTRING::from(title),
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

impl Window {
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
        let network = self.connection.as_ref().map_or_else(String::new, |c| {
            let (lost, sent) = c.lost();
            format!(
                "; {} ms there and back, {:.2}% of packets lost",
                c.round_trip().as_millis(),
                lost as f64 * 100.0 / sent.max(1) as f64
            )
        });
        let title = format!(
            "{} — {}{}x{}, {:.0} fps, {:.1} Mbit/s; behind the newest picture: picture {} ms, \
             {sound}; pictures lead by up to {} ms ([ ]){network}{}",
            self.title,
            if self.ended { "ENDED — " } else { "" },
            self.player.size().0,
            self.player.size().1,
            (now - drawn) as f64 / seconds,
            (total - bytes) as f64 * 8.0 / seconds / 1e6,
            self.player.behind().as_millis(),
            self.player.lead().as_millis(),
            if self.muted { ", muted (M)" } else { "" }
        );
        self.last = (Instant::now(), now, total);
        title
    }

    /// Show pictures a step earlier, or later.
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

/// What the window does once its state is no longer borrowed: these send
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
    let handled = WINDOW.with_borrow_mut(|window| {
        let window = window.as_mut()?;
        Some(match message {
            WM_TIMER => After::Title(window.status()),
            WM_ENDED => {
                window.ended = true;
                After::Title(window.status())
            }
            WM_KEYDOWN if wparam.0 == usize::from(VK_ESCAPE.0) => After::Close,
            WM_KEYDOWN if wparam.0 == usize::from(b'M') => {
                window.muted = !window.muted;
                window.player.set_muted(window.muted);
                After::Title(window.status())
            }
            WM_KEYDOWN if wparam.0 == usize::from(VK_OEM_6.0) => {
                After::Title(window.move_lead(true))
            }
            WM_KEYDOWN if wparam.0 == usize::from(VK_OEM_4.0) => {
                After::Title(window.move_lead(false))
            }
            WM_DESTROY => {
                // Before the window goes: drawing into it fails after.
                window.stop.store(true, Ordering::Relaxed);
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
