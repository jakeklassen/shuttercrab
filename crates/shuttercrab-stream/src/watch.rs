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
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
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
                BLACK_BRUSH, BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW,
                DEFAULT_CHARSET, DRAW_TEXT_FORMAT, DT_BOTTOM, DT_CENTER, DT_SINGLELINE, DT_TOP,
                DT_WORDBREAK, DeleteObject, DrawTextW, EndPaint, FW_NORMAL, FillRect,
                GetMonitorInfoW, GetStockObject, HBRUSH, HDC, HMONITOR, InvalidateRect,
                MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, MonitorFromWindow,
                OUT_DEFAULT_PRECIS, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor,
                TRANSPARENT,
            },
        },
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx},
            LibraryLoader::GetModuleHandleW,
        },
        UI::{
            HiDpi::{
                AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI,
                SetProcessDpiAwarenessContext,
            },
            Input::KeyboardAndMouse::{VK_ESCAPE, VK_OEM_4, VK_OEM_6},
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, Interface, PCWSTR, w},
};

/// The stream ended.
const WM_ENDED: u32 = WM_APP;

/// The stream was joined, or could not be: posted by the joining thread.
const WM_JOINED: u32 = WM_APP + 1;

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

/// A stream joined and its format read, ready to play.
struct Joined {
    format: LiveFormat,
    pictures: BufReader<In>,
    sound: Option<In>,
    connection: Option<Connection>,
    socket: Option<TcpStream>,
}

impl Joined {
    /// Wait for `input`'s format, which comes once the sender has started.
    fn read(input: Input) -> Result<Self> {
        let Input {
            pictures,
            sound,
            connection,
            socket,
        } = input;
        let mut pictures = BufReader::new(pictures);
        let format = LiveFormat::read_from(&mut pictures)?;
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
        Ok(Self {
            format,
            pictures,
            sound,
            connection,
            socket,
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

/// The threads playing a stream, and what stops them.
struct Playing {
    drawing: JoinHandle<()>,
    readers: Vec<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    socket: Option<TcpStream>,
}

impl Playing {
    /// Stop drawing, then close the connection, which ends the readers,
    /// and wait for them, so the player and its sound are let go here,
    /// before the process ends, rather than mid-call on another thread as
    /// it does.
    fn finish(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.drawing.join();
        WINDOW.with_borrow_mut(Option::take);
        if let Some(socket) = self.socket {
            let _ = socket.shutdown(Shutdown::Both);
        }
        for reader in self.readers {
            let _ = reader.join();
        }
    }
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
    /// What the window says until it has pictures: a headline and a line
    /// below it.
    static NOTICE: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

/// Play the stream `join` opens in a window until it is closed: Escape
/// closes it, M mutes, [ and ] show the pictures 20 ms earlier or later
/// against the sound.
///
/// The window opens at once and says it is joining while `join` runs on a
/// thread of its own, so the viewer sees something straight away. If the
/// stream cannot be joined or played, the window says why until it is
/// closed, and this returns `Ok`: the error has been shown, and logged.
pub fn watch(
    join: impl FnOnce() -> Result<Input> + Send + 'static,
    options: &WatchOptions,
) -> Result<()> {
    // Physical pixels, as the app has, and COM for the sound and the
    // decoders. Either may be set up already.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let monitor = options.monitor.unwrap_or_else(pointer_monitor);
    let hwnd = open_window(WAITING_SIZE, monitor, &options.title)?;
    show_notice(hwnd, "Joining the stream…", "Escape stops");

    let (joined, joining) = mpsc::channel();
    let window = hwnd.0 as usize;
    std::thread::spawn(move || {
        // Should the window close first, the stream is let go here.
        if joined.send(join().and_then(Joined::read)).is_ok() {
            let _ = unsafe {
                PostMessageW(
                    Some(HWND(window as *mut _)),
                    WM_JOINED,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
        }
    });

    let mut playing = None;
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            if message.message == WM_JOINED {
                if let Ok(joined) = joining.try_recv() {
                    match joined.and_then(|joined| play(joined, hwnd, options)) {
                        Ok(started) => playing = Some(started),
                        Err(e) => {
                            log::error!("could not join the stream: {e:#}");
                            show_notice(hwnd, "Could not join the stream", &format!("{e:#}"));
                        }
                    }
                }
                continue;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    if let Some(playing) = playing {
        playing.finish();
    }
    Ok(())
}

/// The size the window opens at while it waits for the pictures: most
/// streams are 1080p, so it seldom changes.
const WAITING_SIZE: (u32, u32) = (1920, 1080);

/// Start playing `joined` in the window `hwnd`.
fn play(joined: Joined, hwnd: HWND, options: &WatchOptions) -> Result<Playing> {
    let Joined {
        format,
        pictures,
        sound,
        connection,
        socket,
    } = joined;
    let player = Arc::new(LivePlayer::new(format, |why| {
        log::warn!("the sound stopped: {why}");
    })?);
    player.set_muted(options.muted);
    if (format.width, format.height) != WAITING_SIZE {
        fit_window(hwnd, (format.width, format.height));
    }
    NOTICE.with_borrow_mut(Option::take);

    let window = hwnd.0 as usize;
    let bytes = Arc::new(AtomicU64::new(0));
    let mut readers = Vec::new();
    if let Some(sound) = sound {
        let (player, bytes) = (player.clone(), bytes.clone());
        readers.push(std::thread::spawn(move || {
            read_frames(BufReader::new(sound), &player, &bytes);
        }));
    }
    let (reader_player, reader_bytes) = (player.clone(), bytes.clone());
    readers.push(std::thread::spawn(move || {
        read_frames(pictures, &reader_player, &reader_bytes);
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
        let _ = SetWindowTextW(hwnd, &HSTRING::from(options.title.as_str()));
        SetTimer(Some(hwnd), 1, 500, None);
    }
    Ok(Playing {
        drawing,
        readers,
        stop,
        socket,
    })
}

/// Have the window say `headline`, and `detail` below it, until it has
/// pictures.
fn show_notice(hwnd: HWND, headline: &str, detail: &str) {
    NOTICE.with_borrow_mut(|notice| *notice = Some((headline.into(), detail.into())));
    let _ = unsafe { InvalidateRect(Some(hwnd), None, true) };
}

/// Paint the notice, if there is one: white on black, in the middle.
fn paint_notice(hwnd: HWND) -> bool {
    NOTICE.with_borrow(|notice| {
        let Some((headline, detail)) = notice else {
            return false;
        };
        unsafe {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let mut area = RECT::default();
            let _ = GetClientRect(hwnd, &mut area);
            FillRect(dc, &area, HBRUSH(GetStockObject(BLACK_BRUSH).0));
            SetBkMode(dc, TRANSPARENT);
            let px = |points: i32| points * GetDpiForWindow(hwnd) as i32 / 96;
            let middle = (area.top + area.bottom) / 2;
            let mut above = RECT {
                bottom: middle,
                ..area
            };
            let mut below = RECT {
                left: area.left + px(24),
                top: middle + px(12),
                right: area.right - px(24),
                ..area
            };
            draw_text(
                dc,
                headline,
                (px(28), 0x00F0_F0F0),
                &mut above,
                DT_CENTER | DT_BOTTOM | DT_SINGLELINE,
            );
            draw_text(
                dc,
                detail,
                (px(16), 0x00A0_A0A0),
                &mut below,
                DT_CENTER | DT_TOP | DT_WORDBREAK,
            );
            let _ = EndPaint(hwnd, &paint);
        }
        true
    })
}

/// Draw `text` in Segoe UI `height` pixels high, in `color` (0x00BBGGRR),
/// in `area`.
unsafe fn draw_text(
    dc: HDC,
    text: &str,
    (height, color): (i32, u32),
    area: &mut RECT,
    format: DRAW_TEXT_FORMAT,
) {
    unsafe {
        let font = CreateFontW(
            -height,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        );
        let old = SelectObject(dc, font.into());
        SetTextColor(dc, COLORREF(color));
        let mut text: Vec<u16> = text.encode_utf16().collect();
        DrawTextW(dc, &mut text, area, format);
        SelectObject(dc, old);
        let _ = DeleteObject(font.into());
    }
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

/// The window's style.
const STYLE: WINDOW_STYLE = WS_OVERLAPPEDWINDOW;

/// A window for pictures `size` big, placed by [`placement`].
fn open_window(size: (u32, u32), monitor: HMONITOR, title: &str) -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            // Painted afresh when resized, for the notice to stay in the
            // middle.
            style: CS_HREDRAW | CS_VREDRAW,
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
        let frame = placement(size, monitor)?;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("ShuttercrabLive"),
            &HSTRING::from(title),
            STYLE,
            frame.left,
            frame.top,
            frame.right - frame.left,
            frame.bottom - frame.top,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        Ok(hwnd)
    }
}

/// Size and place the window for pictures `size` big, on the monitor it is
/// on now.
fn fit_window(hwnd: HWND, size: (u32, u32)) {
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
        if let Ok(frame) = placement(size, monitor) {
            let _ = SetWindowPos(
                hwnd,
                None,
                frame.left,
                frame.top,
                frame.right - frame.left,
                frame.bottom - frame.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

/// Where a window for pictures `size` big goes: at their size where it fits
/// on `monitor` and scaled down where it does not, in the middle.
fn placement((width, height): (u32, u32), monitor: HMONITOR) -> Result<RECT> {
    unsafe {
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
        let scale = (room_w / width as f64).min(room_h / height as f64).min(1.0);
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: (width as f64 * scale) as i32,
            bottom: (height as f64 * scale) as i32,
        };
        AdjustWindowRectExForDpi(&mut frame, STYLE, false, WINDOW_EX_STYLE(0), dpi)?;
        let (fw, fh) = (frame.right - frame.left, frame.bottom - frame.top);
        let x = work.left + ((work.right - work.left) - fw) / 2;
        let y = work.top + ((work.bottom - work.top) - fh) / 2;
        Ok(RECT {
            left: x,
            top: y,
            right: x + fw,
            bottom: y + fh,
        })
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
}

extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Escape closes the window whether or not it is playing yet.
    if message == WM_KEYDOWN && wparam.0 == usize::from(VK_ESCAPE.0) {
        let _ = unsafe { DestroyWindow(hwnd) };
        return LRESULT(0);
    }
    if message == WM_PAINT && paint_notice(hwnd) {
        return LRESULT(0);
    }
    let handled = WINDOW.with_borrow_mut(|window| {
        let window = window.as_mut()?;
        Some(match message {
            WM_TIMER => After::Title(window.status()),
            WM_ENDED => {
                window.ended = true;
                After::Title(window.status())
            }
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
    if let Some(After::Title(title)) = handled {
        let _ = unsafe { SetWindowTextW(hwnd, &HSTRING::from(title)) };
        return LRESULT(0);
    }
    if message == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}
