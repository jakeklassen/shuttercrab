//! The windows a screenshot can target under X11: the window manager's
//! client list (EWMH), front to back, with what the user sees of each.

use super::window::{X11, client_bounds, frame_extents, property32, sides, x11};
use crate::targets::{Bounds, WindowTarget};
use shuttercrab_types::WindowId;
use x11rb::{
    properties::WmClass,
    protocol::xproto::{AtomEnum, ConnectionExt as _, MapState, Window},
};

/// Visible top-level windows on the current workspace, front to back.
/// Minimised windows and those on other workspaces are left out.
pub fn visible_windows() -> Vec<WindowTarget> {
    let Some(x) = x11() else {
        return Vec::new();
    };
    let current = property32(
        x,
        x.root,
        x.atoms._NET_CURRENT_DESKTOP,
        AtomEnum::CARDINAL,
        1,
    );
    // Bottom to top.
    let stacking = property32(
        x,
        x.root,
        x.atoms._NET_CLIENT_LIST_STACKING,
        AtomEnum::WINDOW,
        4096,
    );
    stacking
        .into_iter()
        .rev()
        .filter_map(|w| target(x, w, current.first().copied()))
        .collect()
}

/// `window` as a target, if it is on screen: on `current` workspace (or
/// all of them), mapped and not minimised.
fn target(x: &X11, window: Window, current: Option<u32>) -> Option<WindowTarget> {
    let atoms = &x.atoms;
    let attributes = x.conn.get_window_attributes(window).ok()?.reply().ok()?;
    let workspace = property32(x, window, atoms._NET_WM_DESKTOP, AtomEnum::CARDINAL, 1);
    let elsewhere = matches!(
        (workspace.first(), current),
        (Some(&on), Some(current)) if on != current && on != u32::MAX
    );
    let state = property32(x, window, atoms._NET_WM_STATE, AtomEnum::ATOM, 32);
    if attributes.map_state != MapState::VIEWABLE
        || elsewhere
        || state.contains(&atoms._NET_WM_STATE_HIDDEN)
    {
        return None;
    }
    let id = WindowId::from_raw(u64::from(window));
    let (cx, cy, cw, ch) = client_bounds(id).ok()?;
    // The window manager's frame around it, less the invisible shadow a
    // GTK window draws around itself.
    let [left, right, top, bottom] = frame_extents(x, window);
    let [s_left, s_right, s_top, s_bottom] = sides(x, window, atoms._GTK_FRAME_EXTENTS);
    let bounds = Bounds {
        x: cx - left as i32 + s_left as i32,
        y: cy - top as i32 + s_top as i32,
        width: (cw + left + right).saturating_sub(s_left + s_right),
        height: (ch + top + bottom).saturating_sub(s_top + s_bottom),
    };
    if bounds.width == 0 || bounds.height == 0 {
        return None;
    }
    let types = property32(x, window, atoms._NET_WM_WINDOW_TYPE, AtomEnum::ATOM, 8);
    let class = WmClass::get(&x.conn, window)
        .ok()
        .and_then(|c| c.reply().ok().flatten())
        .map(|c| String::from_utf8_lossy(c.class()).into_owned())
        .unwrap_or_default();
    Some(WindowTarget {
        id,
        bounds,
        desktop: types.contains(&atoms._NET_WM_WINDOW_TYPE_DESKTOP),
        class,
    })
}
