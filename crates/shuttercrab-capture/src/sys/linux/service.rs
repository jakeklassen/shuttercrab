//! Screen capture on Linux under X11: the monitors from RandR, each frozen
//! by reading the root window's pixels (`GetImage`), as the compositor
//! shows them. Under Wayland no app may read the screen that way, and
//! capture there is not supported yet.
//!
//! Each request runs on a thread of its own with its own connection, so
//! nothing waits on the UI thread; an X11 connection takes a millisecond.

use crate::screen::{
    CaptureError, CaptureErrorCode, FrozenFrame, MonitorId, MonitorInfo, PhysicalRect, Result,
    Screenshot, WindowId,
};
use futures::channel::oneshot;
use std::future::Future;
use x11rb::{
    connection::Connection,
    protocol::{
        randr::ConnectionExt as _,
        xproto::{ConnectionExt as _, ImageFormat, ImageOrder, Window},
    },
    rust_connection::RustConnection,
};

/// The capture service: requests run on their own threads.
#[derive(Clone)]
pub struct Capture;

impl Capture {
    pub fn start() -> Result<Self> {
        Ok(Self)
    }

    /// Nothing to prepare: X11 needs no devices or shaders.
    pub fn warm_up(&self) {}

    pub fn list_monitors(&self) -> impl Future<Output = Result<Vec<MonitorInfo>>> + use<> {
        on_thread(|| {
            let x = X11::connect()?;
            x.monitors()
        })
    }

    /// Capture `monitor` now. The pointer is never in the image under X11
    /// yet, whatever `include_cursor` says.
    pub fn freeze_monitor(
        &self,
        monitor: MonitorId,
        _include_cursor: bool,
    ) -> impl Future<Output = Result<FrozenFrame>> + use<> {
        on_thread(move || X11::connect()?.freeze(monitor))
    }

    /// Not yet: the app cuts the window from the frozen screen instead.
    pub fn capture_window(
        &self,
        _window: WindowId,
        _include_cursor: bool,
    ) -> impl Future<Output = Result<Screenshot>> + use<> {
        std::future::ready(Err(CaptureError::unsupported("Window capture")))
    }
}

/// The monitor under the pointer, if X11 can say.
pub fn monitor_under_pointer() -> Option<MonitorId> {
    let x = X11::connect().ok()?;
    let pointer = x.conn.query_pointer(x.root).ok()?.reply().ok()?;
    let (px, py) = (i32::from(pointer.root_x), i32::from(pointer.root_y));
    x.monitors().ok()?.into_iter().find_map(|m| {
        let b = m.bounds;
        let inside =
            px >= b.x && py >= b.y && px < b.x + b.width as i32 && py < b.y + b.height as i32;
        inside.then_some(m.id)
    })
}

/// No GPU device to lose.
pub fn is_device_lost(_e: &anyhow::Error) -> bool {
    false
}

/// Run `work` on a thread of its own and hand back its result.
fn on_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> impl Future<Output = Result<T>> {
    let (reply, answer) = oneshot::channel();
    let started = std::thread::Builder::new()
        .name("shuttercrab-capture".into())
        .spawn(move || {
            let _ = reply.send(work());
        });
    async move {
        if let Err(e) = started {
            return Err(unavailable("could not start a capture thread", e));
        }
        answer
            .await
            .unwrap_or_else(|_| Err(unavailable("the capture thread stopped", "")))
    }
}

fn unavailable(message: &str, detail: impl std::fmt::Display) -> CaptureError {
    CaptureError::new(CaptureErrorCode::CaptureUnavailable, message, detail)
}

/// A connection to the X server and its root window.
struct X11 {
    conn: RustConnection,
    root: Window,
}

impl X11 {
    /// Connect, unless this is a Wayland session: there the X server is
    /// XWayland, which sees only X11 apps' windows.
    fn connect() -> Result<Self> {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            return Err(CaptureError::unsupported("Screen capture under Wayland"));
        }
        let (conn, screen) = RustConnection::connect(None)
            .map_err(|e| unavailable("Could not reach the X server", e))?;
        let root = conn.setup().roots[screen].root;
        Ok(Self { conn, root })
    }

    /// The monitors, from RandR, with the scale the desktop sets
    /// (`Xft.dpi`, as GPUI reads it).
    fn monitors(&self) -> Result<Vec<MonitorInfo>> {
        let failed = |e: &dyn std::fmt::Display| unavailable("Could not list the monitors", e);
        let reply = self
            .conn
            .randr_get_monitors(self.root, true)
            .map_err(|e| failed(&e))?
            .reply()
            .map_err(|e| failed(&e))?;
        let scale = self.scale();
        reply
            .monitors
            .iter()
            .map(|m| {
                let name = self
                    .conn
                    .get_atom_name(m.name)
                    .map_err(|e| failed(&e))?
                    .reply()
                    .map_err(|e| failed(&e))?
                    .name;
                let name = String::from_utf8_lossy(&name).into_owned();
                Ok(MonitorInfo {
                    id: MonitorId::from_raw(u64::from(m.name)),
                    device_name: name.clone(),
                    name,
                    bounds: PhysicalRect::new(
                        i32::from(m.x),
                        i32::from(m.y),
                        u32::from(m.width),
                        u32::from(m.height),
                    ),
                    scale_factor: scale,
                    advanced_color_enabled: false,
                    hdr_enabled: false,
                    sdr_white_level_nits: None,
                    adapter: String::new(),
                })
            })
            .collect()
    }

    /// Physical pixels per logical pixel: `Xft.dpi` over 96, or 1.
    fn scale(&self) -> f32 {
        x11rb::resource_manager::new_from_default(&self.conn)
            .ok()
            .and_then(|db| db.get_value::<f32>("Xft.dpi", "").ok().flatten())
            .map_or(1.0, |dpi| dpi / 96.0)
    }

    /// `monitor` now, as BGRA.
    fn freeze(&self, id: MonitorId) -> Result<FrozenFrame> {
        let monitor = self
            .monitors()?
            .into_iter()
            .find(|m| m.id == id)
            .ok_or_else(|| {
                CaptureError::new(
                    CaptureErrorCode::MonitorGone,
                    "That monitor is no longer attached",
                    format!("{id:?}"),
                )
            })?;
        let b = monitor.bounds;
        let failed = |e: &dyn std::fmt::Display| unavailable("Could not capture the screen", e);
        let image = self
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.root,
                b.x as i16,
                b.y as i16,
                b.width as u16,
                b.height as u16,
                !0,
            )
            .map_err(|e| failed(&e))?
            .reply()
            .map_err(|e| failed(&e))?;
        let bgra = to_bgra(
            image.data,
            image.depth,
            self.conn.setup(),
            (b.width, b.height),
        )?;
        Ok(FrozenFrame {
            monitor,
            width: b.width,
            height: b.height,
            bgra,
            // SDR: no HDR content to tone map.
            frame_peak: 1.0,
            hdr_regions: 0,
        })
    }
}

/// An image `GetImage` returned as tightly packed BGRA8. Desktops are
/// depth 24 or 32 at 32 bits a pixel, least significant byte first: blue,
/// green, red and a byte that is not alpha.
fn to_bgra(
    mut data: Vec<u8>,
    depth: u8,
    setup: &x11rb::protocol::xproto::Setup,
    (width, height): (u32, u32),
) -> Result<Vec<u8>> {
    let format = setup.pixmap_formats.iter().find(|f| f.depth == depth);
    let fits = format.is_some_and(|f| f.bits_per_pixel == 32)
        && setup.image_byte_order == ImageOrder::LSB_FIRST
        && data.len() == width as usize * height as usize * 4;
    if !fits {
        return Err(CaptureError::unsupported(&format!("A {depth}-bit screen")));
    }
    for pixel in data.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    Ok(data)
}
