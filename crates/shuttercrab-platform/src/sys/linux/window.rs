//! Window behaviour GPUI does not expose, under X11: a window's X11 id is
//! its [`WindowId`], and these are plain X11 and EWMH requests on a
//! connection of Shuttercrab's own. Under Wayland, where an app cannot
//! place or raise its windows, they do nothing.

use anyhow::{Context, Result, anyhow, bail};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use shuttercrab_types::{MonitorId, WindowId};
use std::sync::OnceLock;
use x11rb::{
    CURRENT_TIME,
    connection::Connection,
    protocol::{
        randr::ConnectionExt as _,
        xproto::{
            AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, EventMask,
            InputFocus, MapState, StackMode, Window,
        },
    },
    rust_connection::RustConnection,
};

x11rb::atom_manager! {
    pub(super) Atoms: AtomsCookie {
        _GTK_FRAME_EXTENTS,
        _NET_ACTIVE_WINDOW,
        _NET_CLIENT_LIST_STACKING,
        _NET_CURRENT_DESKTOP,
        _NET_FRAME_EXTENTS,
        _NET_WM_DESKTOP,
        _NET_WM_STATE,
        _NET_WM_STATE_HIDDEN,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_DESKTOP,
        _NET_WORKAREA,
    }
}

/// Shuttercrab's own X11 connection, its root window and atoms.
pub(super) struct X11 {
    pub(super) conn: RustConnection,
    pub(super) root: Window,
    pub(super) atoms: Atoms,
}

/// The connection, made once; `None` under Wayland or without an X server.
pub(super) fn x11() -> Option<&'static X11> {
    static X: OnceLock<Option<X11>> = OnceLock::new();
    X.get_or_init(|| {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            return None;
        }
        let (conn, screen) = RustConnection::connect(None).ok()?;
        let root = conn.setup().roots[screen].root;
        let atoms = Atoms::new(&conn).ok()?.reply().ok()?;
        Some(X11 { conn, root, atoms })
    })
    .as_ref()
}

fn x11_or_err() -> Result<&'static X11> {
    x11().ok_or_else(|| anyhow!("window placement needs X11"))
}

fn xid(window: WindowId) -> Window {
    window.raw() as Window
}

/// The window behind a GPUI window (or anything else with a native
/// handle), if it is an X11 one.
pub fn of(window: &impl HasWindowHandle) -> Option<WindowId> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xcb(xcb) => Some(WindowId::from_raw(u64::from(xcb.window.get()))),
        RawWindowHandle::Xlib(xlib) => Some(WindowId::from_raw(xlib.window)),
        _ => None,
    }
}

/// The window's drawable area, physical root-window pixels:
/// `(x, y, width, height)`.
pub fn client_bounds(window: WindowId) -> Result<(i32, i32, u32, u32)> {
    let x = x11_or_err()?;
    let w = xid(window);
    let geometry = x.conn.get_geometry(w)?.reply()?;
    let at = x.conn.translate_coordinates(w, x.root, 0, 0)?.reply()?;
    Ok((
        i32::from(at.dst_x),
        i32::from(at.dst_y),
        u32::from(geometry.width),
        u32::from(geometry.height),
    ))
}

/// The 32-bit values of `window`'s `property`, at most `len` of them; none
/// if it has no such property.
pub(super) fn property32(
    x: &X11,
    window: Window,
    property: u32,
    kind: AtomEnum,
    len: u32,
) -> Vec<u32> {
    x.conn
        .get_property(false, window, property, kind, 0, len)
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|p| p.value32().map(Iterator::collect))
        .unwrap_or_default()
}

/// The four sides `property` gives: left, right, top, bottom; zeros if the
/// window has none.
pub(super) fn sides(x: &X11, window: Window, property: u32) -> [u32; 4] {
    <[u32; 4]>::try_from(property32(x, window, property, AtomEnum::CARDINAL, 4)).unwrap_or_default()
}

/// The window manager's frame around the window: left, right, top, bottom.
pub(super) fn frame_extents(x: &X11, window: Window) -> [u32; 4] {
    sides(x, window, x.atoms._NET_FRAME_EXTENTS)
}

/// The window's outer bounds, frame included, physical root-window pixels:
/// `(x, y, width, height)`, as [`place`] takes them.
pub fn outer_bounds(window: WindowId) -> Result<(i32, i32, u32, u32)> {
    let x = x11_or_err()?;
    let (cx, cy, cw, ch) = client_bounds(window)?;
    let [left, right, top, bottom] = frame_extents(x, xid(window));
    Ok((
        cx - left as i32,
        cy - top as i32,
        cw + left + right,
        ch + top + bottom,
    ))
}

/// Put the window's outer bounds at `bounds`, as [`outer_bounds`] gave them.
pub fn place(window: WindowId, (bx, by, width, height): (i32, i32, u32, u32)) -> Result<()> {
    let x = x11_or_err()?;
    let [left, right, top, bottom] = frame_extents(x, xid(window));
    let aux = ConfigureWindowAux::new()
        .x(bx)
        .y(by)
        .width(width.saturating_sub(left + right).max(1))
        .height(height.saturating_sub(top + bottom).max(1));
    x.conn.configure_window(xid(window), &aux)?;
    x.conn.flush()?;
    Ok(())
}

/// Cover exactly `width` × `height` physical pixels at `(x, y)`, above
/// other windows: the selection overlay over its monitor.
pub fn cover(window: WindowId, bx: i32, by: i32, width: u32, height: u32) -> Result<()> {
    let x = x11_or_err()?;
    let aux = ConfigureWindowAux::new()
        .x(bx)
        .y(by)
        .width(width)
        .height(height)
        .stack_mode(StackMode::ABOVE);
    x.conn.configure_window(xid(window), &aux)?;
    x.conn.map_window(xid(window))?;
    x.conn.flush()?;
    Ok(())
}

/// Raise the window and give it the keyboard: ask the window manager to
/// activate it, and set the focus directly for windows it does not manage
/// (the overlay).
pub fn bring_to_front(window: WindowId) {
    let Some(x) = x11() else { return };
    let w = xid(window);
    // Source 2: a pager or the like, which window managers honour.
    let activate = ClientMessageEvent::new(32, w, x.atoms._NET_ACTIVE_WINDOW, [2, 0, 0, 0, 0]);
    let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
    let _ = x.conn.send_event(false, x.root, mask, activate);
    let _ = x
        .conn
        .configure_window(w, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
    let _ = x.conn.set_input_focus(InputFocus::PARENT, w, CURRENT_TIME);
    let _ = x.conn.flush();
}

/// The window that has the keyboard, to give it back later with
/// [`bring_to_front`].
pub fn foreground_window() -> Option<WindowId> {
    let x = x11()?;
    let reply = x
        .conn
        .get_property(
            false,
            x.root,
            x.atoms._NET_ACTIVE_WINDOW,
            AtomEnum::WINDOW,
            0,
            1,
        )
        .ok()?
        .reply()
        .ok()?;
    let active = reply.value32()?.next()?;
    (active != 0).then(|| WindowId::from_raw(u64::from(active)))
}

pub fn hide(window: WindowId) {
    let Some(x) = x11() else { return };
    let _ = x.conn.unmap_window(xid(window));
    let _ = x.conn.flush();
}

pub fn show_normal(window: WindowId) {
    let Some(x) = x11() else { return };
    let _ = x.conn.map_window(xid(window));
    let _ = x.conn.flush();
}

/// X11 cannot keep a window out of other apps' captures.
pub fn exclude_from_capture(_window: WindowId) -> Result<()> {
    bail!("X11 cannot keep a window out of captures")
}

pub fn include_in_capture(_window: WindowId) {}

pub fn is_on_screen(window: WindowId) -> bool {
    let Some(x) = x11() else { return false };
    x.conn
        .get_window_attributes(xid(window))
        .ok()
        .and_then(|c| c.reply().ok())
        .is_some_and(|a| a.map_state == MapState::VIEWABLE)
}

/// The window manager draws the corners.
pub fn round_corners(_window: WindowId) {}

/// GPUI opens such windows without taking the keyboard already.
pub fn never_activate(_window: WindowId) {}

/// No process tree to record the sound of yet.
pub fn sound_process(_window: WindowId) -> Option<u32> {
    None
}

/// The monitors' bounds, by their RandR name atom (the raw [`MonitorId`]).
fn monitors(x: &X11) -> Vec<(MonitorId, (i32, i32, u32, u32))> {
    let Some(reply) = x
        .conn
        .randr_get_monitors(x.root, true)
        .ok()
        .and_then(|c| c.reply().ok())
    else {
        return Vec::new();
    };
    reply
        .monitors
        .iter()
        .map(|m| {
            let bounds = (
                i32::from(m.x),
                i32::from(m.y),
                u32::from(m.width),
                u32::from(m.height),
            );
            (MonitorId::from_raw(u64::from(m.name)), bounds)
        })
        .collect()
}

/// The monitor most of the window is on: the one its centre is on.
pub fn monitor_of(window: WindowId) -> MonitorId {
    let Some(x) = x11() else {
        return MonitorId::from_raw(0);
    };
    let Ok((wx, wy, ww, wh)) = client_bounds(window) else {
        return MonitorId::from_raw(0);
    };
    let (cx, cy) = (wx + ww as i32 / 2, wy + wh as i32 / 2);
    let all = monitors(x);
    all.iter()
        .find(|(_, (mx, my, mw, mh))| {
            cx >= *mx && cy >= *my && cx < mx + *mw as i32 && cy < my + *mh as i32
        })
        .or(all.first())
        .map_or(MonitorId::from_raw(0), |(id, _)| *id)
}

/// The part of `monitor` not covered by panels (physical pixels, root
/// coordinates): x, y, width, height. EWMH gives one work area for the
/// whole desktop; its part on the monitor is close enough.
pub fn work_area(monitor: MonitorId) -> Option<(i32, i32, u32, u32)> {
    let x = x11()?;
    let (mx, my, mw, mh) = monitors(x).into_iter().find(|(id, _)| *id == monitor)?.1;
    let area = property32(x, x.root, x.atoms._NET_WORKAREA, AtomEnum::CARDINAL, 4);
    let Ok([ax, ay, aw, ah]) = <[u32; 4]>::try_from(area) else {
        return Some((mx, my, mw, mh));
    };
    let (ax, ay) = (ax as i32, ay as i32);
    let left = mx.max(ax);
    let top = my.max(ay);
    let right = (mx + mw as i32).min(ax + aw as i32);
    let bottom = (my + mh as i32).min(ay + ah as i32);
    if right <= left || bottom <= top {
        return Some((mx, my, mw, mh));
    }
    Some((left, top, (right - left) as u32, (bottom - top) as u32))
}

/// Size the window's drawable area to `width` × `height` physical pixels,
/// with the whole window at most `share` of its monitor's work area across
/// and down, but its client area at least `least` where the work area
/// allows. It keeps its centre, moved only as far as needed to stay on
/// that monitor.
pub fn fit_client_area(
    window: WindowId,
    (width, height): (u32, u32),
    share: f32,
    least: (u32, u32),
) -> Result<()> {
    let x = x11_or_err()?;
    let (cx, cy, cw, ch) = client_bounds(window)?;
    let (ax, ay, aw, ah) = work_area(monitor_of(window)).context("no work area")?;
    let [left, right, top, bottom] = frame_extents(x, xid(window));
    let (frame_w, frame_h) = (left + right, top + bottom);
    let most = |side: u32, frame: u32| ((side as f32 * share) as u32).saturating_sub(frame);
    let fit = |want: u32, least: u32, most: u32| want.min(most).max(least.min(most)).max(1);
    let w = fit(width, least.0, most(aw, frame_w));
    let h = fit(height, least.1, most(ah, frame_h));
    // Keep the centre, but stay inside the work area.
    let centre = (cx + cw as i32 / 2, cy + ch as i32 / 2);
    let keep = |centre: i32, side: u32, start: i32, room: u32, before: u32, after: u32| {
        let lowest = start + before as i32;
        let highest = start + room as i32 - side as i32 - after as i32;
        (centre - side as i32 / 2).clamp(lowest, highest.max(lowest))
    };
    let nx = keep(centre.0, w, ax, aw, left, right);
    let ny = keep(centre.1, h, ay, ah, top, bottom);
    let aux = ConfigureWindowAux::new().x(nx).y(ny).width(w).height(h);
    x.conn.configure_window(xid(window), &aux)?;
    x.conn.flush()?;
    Ok(())
}
