//! The watching side: a stream opened, and the window that plays it.
//!
//! - [`input`]: where the stream comes from, and joining it.
//! - [`notice`]: what the viewer tells the user, before the pictures and
//!   over them.
//! - [`window`]: the Win32 window that shows the pictures.

mod input;
mod notice;
mod window;

pub use input::Input;
pub use notice::tell;

use anyhow::Result;
use input::Joined;
use notice::{NOTICE, Notices, Over, Overlay, show_notice};
use shuttercrab_capture::live::{LiveFrame, LivePlayer};
use std::{
    io::{BufReader, Read},
    net::{Shutdown, TcpStream},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use window::{WINDOW, WM_ENDED, Window, fit_window, open_window, pointer_monitor, swap_chain};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        Graphics::{
            Direct3D11::ID3D11Texture2D,
            Dxgi::{Common::DXGI_FORMAT_UNKNOWN, DXGI_PRESENT},
            Gdi::HMONITOR,
        },
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx},
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
            WindowsAndMessaging::*,
        },
    },
    core::HSTRING,
};

/// The stream was joined, or could not be: posted by the joining thread.
const WM_JOINED: u32 = WM_APP + 1;

/// How to show a stream.
#[derive(Clone, Debug, Default)]
pub struct WatchOptions {
    /// The monitor to open on: the one under the pointer if none.
    pub monitor: Option<HMONITOR>,
    pub muted: bool,
    /// The sound's volume to start at, as a percentage of its level as
    /// sent, up to `MAX_VOLUME`: 100 if none.
    pub volume: Option<u32>,
    /// Told each volume chosen, to keep for next time.
    pub remember_volume: Option<fn(u32)>,
    /// What the window's title starts with.
    pub title: String,
}

/// The loudest the viewer plays the sound, as a percentage of its level as
/// sent: four times as loud, held under clipping.
pub const MAX_VOLUME: u32 = 400;

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

/// Play the stream `join` opens in a window until it is closed: Escape
/// closes it, M mutes, \[ and \] show the pictures 20 ms earlier or later
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
        // 0 when quitting, -1 if it fails: either way, the loop is over.
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
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
    let volume = options.volume.unwrap_or(100).min(MAX_VOLUME);
    player.set_volume(volume);
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
    let over = Over::default();
    let drawing = {
        let (player, drawn, stop, over, title) = (
            player.clone(),
            drawn.clone(),
            stop.clone(),
            over.clone(),
            options.title.clone(),
        );
        std::thread::spawn(move || {
            let hwnd = HWND(window as *mut _);
            let size = (format.width, format.height);
            if let Err(e) = draw(&player, hwnd, size, (&drawn, &stop), &over)
                && !stop.load(Ordering::Relaxed)
            {
                // The last picture would stay, as if the stream had frozen.
                log::error!("drawing stopped: {e:#}");
                tell(
                    &title,
                    &format!("The viewer could not show the stream any more: {e:#}"),
                );
                let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
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
            volume,
            remember_volume: options.remember_volume,
            over: over.clone(),
            ended: None,
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
            Ok(None) => {
                log::info!("the stream ended");
                break;
            }
            Err(e) => {
                log::warn!("the stream broke: {e:#}");
                break;
            }
        }
    }
}

/// Draw each picture as it falls due, once a refresh of the display the
/// window is on, until `stop`; or what is `over` them, once there is
/// something.
fn draw(
    player: &LivePlayer,
    hwnd: HWND,
    (width, height): (u32, u32),
    (drawn, stop): (&AtomicU64, &AtomicBool),
    over: &Mutex<Overlay>,
) -> Result<()> {
    let swap_chain = swap_chain(&player.device(), hwnd, width, height)?;
    let mut size = (width, height);
    let mut output = None;
    let mut refreshes = 0u32;
    // Made the first time there is a notice, and the notice last drawn.
    let (mut notices, mut shown) = (None, None);
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
        // Drawn into the frames rather than painted on the window: once a
        // window has shown pictures, what it paints stays behind the last.
        let Overlay { notice, badge } = lock(over).clone();
        if let Some(notice) = notice {
            if shown.as_ref() != Some(&notice) {
                let notices = match &mut notices {
                    Some(notices) => notices,
                    None => notices.insert(Notices::new()?),
                };
                notices.draw(&back, &notice)?;
                unsafe { swap_chain.Present(0, DXGI_PRESENT(0)) }.ok()?;
                shown = Some(notice);
            }
            continue;
        }
        if player.draw(&back)? {
            // On each new picture while it lasts: the frames are drawn whole.
            if let Some((label, _)) = badge.filter(|(_, until)| Instant::now() < *until) {
                let notices = match &mut notices {
                    Some(notices) => notices,
                    None => notices.insert(Notices::new()?),
                };
                notices.badge(&back, &label)?;
            }
            // Just after a refresh: shown at the next.
            unsafe { swap_chain.Present(0, DXGI_PRESENT(0)) }.ok()?;
            drawn.fetch_add(1, Ordering::Relaxed);
        }
    }
    Ok(())
}

/// `mutex`'s value, even if a thread panicked holding it: a notice is only
/// text.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
