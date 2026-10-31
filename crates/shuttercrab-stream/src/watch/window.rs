//! The viewer's Win32 window: opened and placed on a monitor, its keys and
//! its timer, and the swap chain the pictures are drawn into.

use super::{
    MAX_VOLUME, lock,
    notice::{Over, paint_notice},
};
use anyhow::{Context, Result, ensure};
use shuttercrab_capture::live::LivePlayer;
use shuttercrab_connect::Connection;
use std::{
    cell::RefCell,
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
            Direct3D11::ID3D11Device,
            Dxgi::{
                Common::{DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_DISCARD,
                DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2, IDXGISwapChain1,
            },
            Gdi::{
                GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
                MonitorFromWindow,
            },
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{AdjustWindowRectExForDpi, GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            Input::KeyboardAndMouse::{VK_DOWN, VK_ESCAPE, VK_OEM_4, VK_OEM_6, VK_UP},
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, Interface, PCWSTR, w},
};

/// The stream ended.
pub(super) const WM_ENDED: u32 = WM_APP;

/// How long a window whose stream has ended stays open, saying so.
const CLOSE_AFTER_ENDED: Duration = Duration::from_secs(10);

/// How much \[ and \] change how far ahead of the sound pictures are shown.
const LEAD_STEP: Duration = Duration::from_millis(20);

/// How much a key press or a notch of the wheel changes the volume.
const VOLUME_STEP: u32 = 10;

/// How long a badge stays over the pictures.
const BADGE_FOR: Duration = Duration::from_millis(1500);

/// The window's state, on its thread. The pictures are drawn on a thread of
/// their own, so the window keeps up while they are.
pub(super) struct Window {
    pub(super) player: Arc<LivePlayer>,
    pub(super) bytes: Arc<AtomicU64>,
    /// Pictures drawn, counted by the drawing thread.
    pub(super) drawn: Arc<AtomicU64>,
    /// Tells the drawing thread to stop.
    pub(super) stop: Arc<AtomicBool>,
    pub(super) last: (Instant, u64, u64),
    pub(super) muted: bool,
    /// A percentage of the sound's level as sent.
    pub(super) volume: u32,
    pub(super) remember_volume: Option<fn(u32)>,
    /// What to draw over the pictures instead of them, if anything: a
    /// headline and a line below it.
    pub(super) over: Over,
    /// When the stream ended, if it has: the window closes
    /// [`CLOSE_AFTER_ENDED`] later.
    pub(super) ended: Option<Instant>,
    pub(super) title: String,
    /// The connection to the stream, if it is online.
    pub(super) connection: Option<Connection>,
}

thread_local! {
    pub(super) static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// The monitor under the pointer.
pub(super) fn pointer_monitor() -> HMONITOR {
    let mut at = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut at);
        MonitorFromPoint(at, MONITOR_DEFAULTTOPRIMARY)
    }
}

/// The window's style.
const STYLE: WINDOW_STYLE = WS_OVERLAPPEDWINDOW;

/// A window for pictures `size` big, placed by [`placement`].
pub(super) fn open_window(size: (u32, u32), monitor: HMONITOR, title: &str) -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            // Painted afresh when resized, for the notice to stay in the
            // middle.
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            // The viewer's own icon, resource 2, so its taskbar button stands
            // apart from the app's; else the program's, resource 1, if it
            // has one. Windows takes the small size from it too.
            hIcon: LoadIconW(Some(instance.into()), PCWSTR(2 as _))
                .or_else(|_| LoadIconW(Some(instance.into()), PCWSTR(1 as _)))
                .unwrap_or_default(),
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
pub(super) fn fit_window(hwnd: HWND, size: (u32, u32)) {
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
pub(super) fn swap_chain(
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
    /// Raise or lower the volume by `steps` of [`VOLUME_STEP`], within 0 and
    /// [`MAX_VOLUME`], and keep it for next time.
    fn change_volume(&mut self, steps: i32) -> After {
        let volume = (self.volume as i32 + steps * VOLUME_STEP as i32).clamp(0, MAX_VOLUME as i32);
        if volume as u32 != self.volume {
            self.volume = volume as u32;
            self.player.set_volume(self.volume);
            if let Some(remember) = self.remember_volume {
                remember(self.volume);
            }
        }
        // Turning it up unmutes, as on a TV.
        if self.muted && steps > 0 {
            self.muted = false;
            self.player.set_muted(false);
        }
        self.show_volume();
        After::Title(self.status())
    }

    /// Show the volume for a moment over the pictures.
    fn show_volume(&self) {
        let label = if self.muted {
            "Muted".to_string()
        } else {
            format!("Volume {}%", self.volume)
        };
        lock(&self.over).badge = Some((label, Instant::now() + BADGE_FOR));
    }

    /// The stream has ended: say so over the pictures, with the time left
    /// before the window closes, or close it.
    fn count_down(&self) -> After {
        let ended = self.ended.map_or(Duration::ZERO, |at| at.elapsed());
        let Some(left) = CLOSE_AFTER_ENDED
            .checked_sub(ended)
            .filter(|l| !l.is_zero())
        else {
            return After::Close;
        };
        lock(&self.over).notice = Some((
            "The stream has ended".into(),
            format!(
                "This window closes in {} s. Escape closes it now.",
                left.as_secs_f64().ceil()
            ),
        ));
        After::Title(format!("{} — the stream has ended", self.title))
    }

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
            "{} — {}x{}, {:.0} fps, {:.1} Mbit/s; behind the newest picture: picture {} ms, \
             {sound}; pictures lead by up to {} ms ([ ]){network}{}",
            self.title,
            self.player.size().0,
            self.player.size().1,
            (now - drawn) as f64 / seconds,
            (total - bytes) as f64 * 8.0 / seconds / 1e6,
            self.player.behind().as_millis(),
            self.player.lead().as_millis(),
            if self.muted {
                ", muted (M)".to_string()
            } else {
                format!(", volume {}% (Up, Down)", self.volume)
            }
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
            WM_TIMER if window.ended.is_some() => window.count_down(),
            WM_TIMER => After::Title(window.status()),
            WM_ENDED => {
                // The last picture would stay, as if the window had frozen:
                // the notice takes its place.
                window.ended.get_or_insert_with(Instant::now);
                window.count_down()
            }
            WM_KEYDOWN if wparam.0 == usize::from(b'M') => {
                window.muted = !window.muted;
                window.player.set_muted(window.muted);
                window.show_volume();
                After::Title(window.status())
            }
            WM_KEYDOWN if wparam.0 == usize::from(VK_UP.0) => window.change_volume(1),
            WM_KEYDOWN if wparam.0 == usize::from(VK_DOWN.0) => window.change_volume(-1),
            // A notch is 120, up or down.
            WM_MOUSEWHEEL => window.change_volume(i32::from((wparam.0 >> 16) as i16) / 120),
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
