//! Watching one window while it is recorded: Windows says when it moves or
//! changes size, when it is dragged, when it is minimised, hidden or shown
//! again, and when it is closed (`SetWinEventHook`, out of context, so the
//! events arrive on the platform thread's message loop; nothing is
//! polled).

use super::platform::with_state;
use crate::{PlatformEvent, WindowChange, frame::Rect};
use std::cell::RefCell;
use windows::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
    UI::{
        Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
        WindowsAndMessaging::{
            CHILDID_SELF, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE,
            EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_MINIMIZEEND,
            EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZESTART,
            GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, OBJID_WINDOW,
            WINEVENT_OUTOFCONTEXT,
        },
    },
};

/// Whether the window can be seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    Shown,
    Minimized,
    Hidden,
    Closed,
}

impl Seen {
    /// The window as it is now. Each event checks the window itself:
    /// Windows' events alone are not to be trusted (one says "restored" as
    /// the minimise animation ends).
    fn now(hwnd: HWND) -> Self {
        unsafe {
            if !IsWindow(Some(hwnd)).as_bool() {
                Self::Closed
            } else if IsIconic(hwnd).as_bool() {
                Self::Minimized
            } else if !IsWindowVisible(hwnd).as_bool() || cloaked(hwnd) {
                Self::Hidden
            } else {
                Self::Shown
            }
        }
    }
}

/// Whether DWM keeps `hwnd` off screen (a suspended app, or a window on
/// another virtual desktop).
fn cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            size_of::<u32>() as u32,
        )
    }
    .is_ok()
        && cloaked != 0
}

/// The window watched, and its hooks.
struct Watched {
    hwnd: HWND,
    hooks: Vec<HWINEVENTHOOK>,
    /// Its bounds as last reported, so a repeat is not sent again.
    last: Option<Rect>,
    /// Whether it could be seen when last checked.
    seen: Seen,
}

thread_local! {
    static WATCHED: RefCell<Option<Watched>> = const { RefCell::new(None) };
}

/// Watch `window` (an `HWND`) in place of the one watched before, or stop
/// watching with `None`. Runs on the platform thread.
pub(crate) fn watch(window: Option<isize>) {
    WATCHED.with(|watched| {
        if let Some(old) = watched.borrow_mut().take() {
            for hook in old.hooks {
                let _ = unsafe { UnhookWinEvent(hook) };
            }
        }
        let Some(window) = window else {
            return;
        };
        let hwnd = HWND(window as _);
        let mut process = 0;
        if unsafe { GetWindowThreadProcessId(hwnd, Some(&mut process)) } == 0 {
            log::warn!("the window to watch is gone");
            return;
        }
        // Only its process's events, so the rest of the desktop's moving
        // carets and pointers never wake this thread.
        let ranges = [
            (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
            (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
            (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            (EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND),
        ];
        let hooks = ranges
            .into_iter()
            .filter_map(|(first, last)| {
                let hook = unsafe {
                    SetWinEventHook(
                        first,
                        last,
                        None,
                        Some(on_event),
                        process,
                        0,
                        WINEVENT_OUTOFCONTEXT,
                    )
                };
                (!hook.is_invalid()).then_some(hook)
            })
            .collect::<Vec<_>>();
        if hooks.len() < ranges.len() {
            log::warn!("could not watch the recorded window fully; the border may not follow it");
        }
        *watched.borrow_mut() = Some(Watched {
            hwnd,
            hooks,
            last: None,
            seen: Seen::now(hwnd),
        });
    });
}

/// `window`'s visible bounds, without the invisible resize border and the
/// shadow.
pub fn visible_bounds(window: isize) -> Option<Rect> {
    let mut rect = RECT::default();
    unsafe {
        DwmGetWindowAttribute(
            HWND(window as _),
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut rect).cast(),
            size_of::<RECT>() as u32,
        )
    }
    .ok()?;
    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
    (width > 0 && height > 0).then(|| Rect::new(rect.left, rect.top, width as u32, height as u32))
}

unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    if object != OBJID_WINDOW.0 || child != CHILDID_SELF as i32 {
        return;
    }
    let change = WATCHED.with(|watched| {
        let mut watched = watched.borrow_mut();
        let watched = watched.as_mut().filter(|w| w.hwnd == hwnd)?;
        match event {
            EVENT_SYSTEM_MOVESIZESTART => return Some(WindowChange::DragStarted),
            EVENT_SYSTEM_MOVESIZEEND => return Some(WindowChange::DragEnded),
            _ => {}
        }
        // Windows says so while the window is being destroyed, when it
        // still exists for a moment.
        let seen = if event == EVENT_OBJECT_DESTROY {
            Seen::Closed
        } else {
            Seen::now(hwnd)
        };
        if event != EVENT_OBJECT_LOCATIONCHANGE {
            log::debug!("recorded window: event {event:#x}, now {seen:?}");
        }
        if seen != watched.seen {
            // Closed is final: nothing follows it.
            if watched.seen == Seen::Closed {
                return None;
            }
            watched.seen = seen;
            return Some(match seen {
                Seen::Shown => WindowChange::Restored,
                Seen::Minimized => WindowChange::Minimized,
                Seen::Hidden => WindowChange::Hidden,
                Seen::Closed => WindowChange::Closed,
            });
        }
        // Out of sight, it may sit far off screen: the border stays where
        // it was.
        if seen != Seen::Shown || event != EVENT_OBJECT_LOCATIONCHANGE {
            return None;
        }
        let bounds = visible_bounds(hwnd.0 as isize)?;
        (watched.last.replace(bounds) != Some(bounds)).then_some(WindowChange::Moved(bounds))
    });
    if let Some(change) = change {
        with_state(|s| s.events.unbounded_send(PlatformEvent::Window(change)));
    }
}
