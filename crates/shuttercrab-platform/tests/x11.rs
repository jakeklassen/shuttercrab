//! The Linux platform against a real X server: the hotkeys, the clipboard
//! and the windows on screen. They send keys and open windows, so they run
//! only on a private server, never on the desktop in use:
//!
//!   scripts/linux-x11-tests.sh
//!
//! starts one (Xvfb, with openbox for the window tests) and runs them.
#![cfg(target_os = "linux")]

use futures::{channel::mpsc::UnboundedReceiver, executor::block_on};
use shuttercrab_platform::{Hotkey, Platform, PlatformEvent, targets::visible_windows};
use std::{
    path::PathBuf,
    sync::Arc,
    thread::sleep,
    time::{Duration, Instant},
};
use x11rb::{
    COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME,
    connection::Connection,
    protocol::{
        Event,
        xproto::{
            AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, Keycode,
            PropMode, Property, Window, WindowClass,
        },
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

/// Set by the script, which starts the private server.
const PRIVATE_SERVER: &str = "SHUTTERCRAB_TEST_X11";

/// A connection of the test's own, to press keys and paste with. Panics
/// unless the script started the server.
fn connect() -> (RustConnection, Window) {
    assert!(
        std::env::var_os(PRIVATE_SERVER).is_some(),
        "run scripts/linux-x11-tests.sh: these tests press keys and open windows"
    );
    let (conn, screen) = RustConnection::connect(None).expect("an X server");
    let root = conn.setup().roots[screen].root;
    (conn, root)
}

/// The first key that types `keysym`.
fn keycode(conn: &RustConnection, keysym: u32) -> Keycode {
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let mapping = conn
        .get_keyboard_mapping(min, max - min + 1)
        .unwrap()
        .reply()
        .unwrap();
    let per = usize::from(mapping.keysyms_per_keycode);
    let row = mapping
        .keysyms
        .chunks(per)
        .position(|syms| syms.contains(&keysym))
        .unwrap_or_else(|| panic!("no key types {keysym:#x}"));
    min + u8::try_from(row).unwrap()
}

const CONTROL_L: u32 = 0xffe3;
const ALT_L: u32 = 0xffe9;
const CAPS_LOCK: u32 = 0xffe5;
const NUM_LOCK: u32 = 0xff7f;

/// Press `keysyms` in order, and let go in reverse, as XTEST's fake input:
/// the server takes them as typed.
fn chord(conn: &RustConnection, root: Window, keysyms: &[u32]) {
    let keys: Vec<Keycode> = keysyms.iter().map(|&k| keycode(conn, k)).collect();
    for &key in &keys {
        fake(conn, root, key, true);
    }
    for &key in keys.iter().rev() {
        fake(conn, root, key, false);
    }
    conn.sync().unwrap();
}

fn fake(conn: &RustConnection, root: Window, key: Keycode, down: bool) {
    // KeyPress is event type 2, KeyRelease 3.
    let kind = if down { 2 } else { 3 };
    conn.xtest_fake_input(kind, key, CURRENT_TIME, root, 0, 0, 0)
        .unwrap();
}

/// The next event within `wait`.
fn next_event(
    events: &mut UnboundedReceiver<PlatformEvent>,
    wait: Duration,
) -> Option<PlatformEvent> {
    let until = Instant::now() + wait;
    loop {
        if let Ok(event) = events.try_recv() {
            return Some(event);
        }
        if Instant::now() > until {
            return None;
        }
        sleep(Duration::from_millis(10));
    }
}

/// The events within `wait`.
fn events_within(
    events: &mut UnboundedReceiver<PlatformEvent>,
    wait: Duration,
) -> Vec<PlatformEvent> {
    let until = Instant::now() + wait;
    let mut seen = Vec::new();
    while let Some(event) = next_event(events, until.saturating_duration_since(Instant::now())) {
        seen.push(event);
    }
    seen
}

const SECOND: Duration = Duration::from_secs(1);

#[test]
#[ignore = "needs a private X server: scripts/linux-x11-tests.sh"]
fn a_hotkey_fires_whatever_the_locks() {
    let (conn, root) = connect();
    let hotkey = Hotkey::parse("Ctrl+Alt+S").unwrap();
    let (_platform, mut events, conflicts) = Platform::start(&[(1, hotkey)], None).unwrap();
    assert!(conflicts.is_empty(), "{conflicts:?}");
    let s = u32::from(b's');

    chord(&conn, root, &[CONTROL_L, ALT_L, s]);
    assert_eq!(
        next_event(&mut events, SECOND),
        Some(PlatformEvent::Hotkey(1))
    );

    for lock in [CAPS_LOCK, NUM_LOCK] {
        chord(&conn, root, &[lock]);
        chord(&conn, root, &[CONTROL_L, ALT_L, s]);
        chord(&conn, root, &[lock]);
        assert_eq!(
            next_event(&mut events, SECOND),
            Some(PlatformEvent::Hotkey(1)),
            "with lock {lock:#x} on"
        );
    }

    // Other modifiers, or none, are not the hotkey.
    chord(&conn, root, &[ALT_L, s]);
    chord(&conn, root, &[s]);
    assert_eq!(events_within(&mut events, SECOND / 4), []);
}

#[test]
#[ignore = "needs a private X server: scripts/linux-x11-tests.sh"]
fn a_held_hotkey_fires_once() {
    let (conn, root) = connect();
    let hotkey = Hotkey::parse("Ctrl+Alt+H").unwrap();
    let (_platform, mut events, _) = Platform::start(&[(1, hotkey)], None).unwrap();
    let keys = [CONTROL_L, ALT_L, u32::from(b'h')].map(|k| keycode(&conn, k));
    for key in keys {
        fake(&conn, root, key, true);
    }
    // Pressed again while down, as a repeat, and held past the server's
    // own repeat delay.
    fake(&conn, root, keys[2], true);
    fake(&conn, root, keys[2], true);
    conn.sync().unwrap();
    sleep(SECOND);
    for key in keys.into_iter().rev() {
        fake(&conn, root, key, false);
    }
    conn.sync().unwrap();
    assert_eq!(
        events_within(&mut events, SECOND / 2),
        [PlatformEvent::Hotkey(1)]
    );
}

#[test]
#[ignore = "needs a private X server: scripts/linux-x11-tests.sh"]
fn hotkeys_are_replaced_and_conflicts_reported() {
    let (conn, root) = connect();
    let first = Hotkey::parse("Ctrl+Alt+F7").unwrap();
    let second = Hotkey::parse("Ctrl+Alt+Shift+F8").unwrap();
    let (platform, mut events, conflicts) = Platform::start(&[(1, first)], None).unwrap();
    assert!(conflicts.is_empty(), "{conflicts:?}");

    // Another client asking for the same keys is told they are taken.
    let (other, _other_events, conflicts) = Platform::start(&[(7, first)], None).unwrap();
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    assert_eq!(conflicts[0].id, 7);
    drop(other);

    // Replaced: the old keys do nothing, the new ones fire.
    let conflicts = block_on(platform.set_hotkeys(vec![(2, second)]));
    assert!(conflicts.is_empty(), "{conflicts:?}");
    let (f7, f8, shift_l) = (0xffc4, 0xffc5, 0xffe1);
    chord(&conn, root, &[CONTROL_L, ALT_L, f7]);
    chord(&conn, root, &[CONTROL_L, ALT_L, shift_l, f8]);
    assert_eq!(
        events_within(&mut events, SECOND / 2),
        [PlatformEvent::Hotkey(2)]
    );

    // Dropped, its keys are free again.
    drop(platform);
    let (_again, _events, conflicts) = Platform::start(&[(3, second)], None).unwrap();
    assert!(conflicts.is_empty(), "{conflicts:?}");
}

/// What the clipboard gives a pasting app for `target`, as another app
/// asks for it: large data comes in parts (INCR).
fn paste(conn: &RustConnection, root: Window, target: &str) -> Option<Vec<u8>> {
    let atom = |name: &str| {
        conn.intern_atom(false, name.as_bytes())
            .unwrap()
            .reply()
            .unwrap()
            .atom
    };
    let (clipboard, target, property, incr) = (
        atom("CLIPBOARD"),
        atom(target),
        atom("SHUTTERCRAB_PASTE"),
        atom("INCR"),
    );
    let window = conn.generate_id().unwrap();
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        window,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        COPY_FROM_PARENT,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .unwrap();
    conn.convert_selection(window, clipboard, target, property, CURRENT_TIME)
        .unwrap();
    conn.flush().unwrap();
    let until = Instant::now() + 5 * SECOND;
    let read = |delete: bool| {
        conn.get_property(delete, window, property, AtomEnum::ANY, 0, u32::MAX / 4)
            .unwrap()
            .reply()
            .unwrap()
    };
    let mut data = None::<Vec<u8>>;
    let mut incremental = false;
    while Instant::now() < until {
        let Some(event) = conn.poll_for_event().unwrap() else {
            sleep(Duration::from_millis(5));
            continue;
        };
        match event {
            Event::SelectionNotify(notify) if notify.requestor == window => {
                if notify.property == u32::from(AtomEnum::NONE) {
                    break;
                }
                let reply = read(true);
                if reply.type_ == incr {
                    incremental = true;
                    data = Some(Vec::new());
                } else {
                    data = Some(reply.value);
                    break;
                }
            }
            Event::PropertyNotify(change)
                if incremental
                    && change.atom == property
                    && change.state == Property::NEW_VALUE =>
            {
                let reply = read(true);
                if reply.value.is_empty() {
                    break;
                }
                data.get_or_insert_default().extend(reply.value);
            }
            _ => {}
        }
    }
    conn.destroy_window(window).unwrap();
    conn.flush().unwrap();
    data
}

/// Noise, so the PNG does not shrink and a large copy needs INCR.
fn noise(width: u32, height: u32) -> Vec<u8> {
    let mut state = 0x2545_f491_u32;
    (0..width * height)
        .flat_map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let [r, g, b, _] = state.to_le_bytes();
            [r, g, b, 255]
        })
        .collect()
}

fn decode(png_bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
        .read_info()
        .unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    pixels.truncate(info.buffer_size());
    assert_eq!(info.color_type, png::ColorType::Rgba);
    (info.width, info.height, pixels)
}

#[test]
#[ignore = "needs a private X server: scripts/linux-x11-tests.sh"]
fn copies_reach_other_apps() {
    let (conn, root) = connect();
    let (platform, _events, _) = Platform::start(&[], None).unwrap();

    for (width, height) in [(3, 2), (1920, 1080)] {
        let rgba = noise(width, height);
        let unused = Arc::new(Vec::new());
        block_on(platform.copy_image(unused, Arc::new(rgba.clone()), width, height)).unwrap();
        let pasted = paste(&conn, root, "image/png").expect("a PNG on the clipboard");
        assert_eq!(decode(&pasted), (width, height, rgba), "{width}×{height}");
    }

    // A file that exists, as a saved screenshot does, with spaces to
    // escape.
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("shuttercrab test");
    std::fs::create_dir_all(&folder).unwrap();
    let file = folder
        .join("shot 1.png")
        .canonicalize()
        .unwrap_or_else(|_| {
            std::fs::write(folder.join("shot 1.png"), b"").unwrap();
            folder.join("shot 1.png").canonicalize().unwrap()
        });
    block_on(platform.copy_files(vec![file.clone()])).unwrap();
    let list = paste(&conn, root, "text/uri-list").expect("files on the clipboard");
    let list = String::from_utf8(list).unwrap();
    let uri = format!("file://{}", file.display()).replace(' ', "%20");
    assert!(list.lines().any(|line| line == uri), "{list:?} has {uri}");
    std::fs::remove_dir_all(&folder).unwrap();

    // The copy outlives the platform that made it.
    drop(platform);
    assert!(paste(&conn, root, "text/uri-list").is_some());
}

/// A window of `size` at `at`, with a class, shown.
fn open_window(conn: &RustConnection, root: Window, at: (i16, i16), size: (u16, u16)) -> Window {
    let window = conn.generate_id().unwrap();
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        window,
        root,
        at.0,
        at.1,
        size.0,
        size.1,
        0,
        WindowClass::INPUT_OUTPUT,
        COPY_FROM_PARENT,
        &CreateWindowAux::new(),
    )
    .unwrap();
    conn.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        b"shuttercrab-test\0ShuttercrabTest\0",
    )
    .unwrap();
    conn.map_window(window).unwrap();
    conn.sync().unwrap();
    window
}

/// Wait up to five seconds for the window manager to bring the windows on
/// screen to `done`.
fn wait_for(done: impl Fn(&[Window]) -> bool) -> Vec<Window> {
    let until = Instant::now() + 5 * SECOND;
    loop {
        let on_screen: Vec<Window> = visible_windows()
            .iter()
            .map(|w| u32::try_from(w.id.raw()).unwrap())
            .collect();
        if done(&on_screen) || Instant::now() > until {
            return on_screen;
        }
        sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "needs a private X server: scripts/linux-x11-tests.sh"]
fn lists_the_windows_on_screen_front_to_back() {
    let (conn, root) = connect();
    let back = open_window(&conn, root, (100, 100), (300, 200));
    let front = open_window(&conn, root, (500, 300), (400, 250));
    let on_screen = wait_for(|w| w.contains(&back) && w.contains(&front));
    let position = |window| on_screen.iter().position(|&w| w == window);
    assert!(
        position(front).unwrap() < position(back).unwrap(),
        "front to back: {on_screen:?}"
    );

    // Its bounds take in the window manager's frame around it.
    let targets = visible_windows();
    let target = targets
        .iter()
        .find(|t| t.id.raw() == u64::from(front))
        .unwrap();
    assert_eq!(target.class, "ShuttercrabTest");
    assert!(!target.desktop);
    let geometry = conn.get_geometry(front).unwrap().reply().unwrap();
    let inside = conn
        .translate_coordinates(front, root, 0, 0)
        .unwrap()
        .reply()
        .unwrap();
    let bounds = target.bounds;
    assert!(
        bounds.width >= u32::from(geometry.width) && bounds.height > u32::from(geometry.height),
        "{bounds:?} frames {geometry:?}"
    );
    assert!(bounds.contains(i32::from(inside.dst_x), i32::from(inside.dst_y)));

    // Minimised, it is not a target.
    let iconic = 3;
    let change_state = conn
        .intern_atom(false, b"WM_CHANGE_STATE")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let minimise = ClientMessageEvent::new(32, back, change_state, [iconic, 0, 0, 0, 0]);
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        minimise,
    )
    .unwrap();
    conn.sync().unwrap();
    let on_screen = wait_for(|w| !w.contains(&back));
    assert!(!on_screen.contains(&back), "{on_screen:?}");
    assert!(on_screen.contains(&front), "{on_screen:?}");

    for window in [back, front] {
        conn.destroy_window(window).unwrap();
    }
    conn.sync().unwrap();
}
