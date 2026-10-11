//! Global hotkeys under X11: each is a key grab on the root window, made on
//! a connection of their own whose thread waits for the presses. Another
//! app (GNOME's own shortcuts among them) that grabbed the same keys first
//! makes the grab fail, which is the hotkey's conflict.
//!
//! Wayland lets no app grab keys everywhere; there, desktop shortcuts run
//! Shuttercrab's requests instead.

use crate::{Hotkey, HotkeyConflict, Key, PlatformEvent};
use anyhow::{Context, Result};
use futures::channel::mpsc::UnboundedSender;
use std::sync::{Arc, Mutex, PoisonError};
use x11rb::{
    COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT,
    connection::Connection,
    errors::ReplyError,
    protocol::{
        ErrorKind, Event,
        xkb::{self, ConnectionExt as _, PerClientFlag},
        xproto::{
            AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, GrabMode,
            Keycode, Mapping, ModMask, Window, WindowClass,
        },
    },
    rust_connection::RustConnection,
};

/// Whether Print Screen belongs to the system. Under X11 a grab that GNOME
/// already holds fails like any other conflict, so nothing is assumed.
pub fn print_screen_taken() -> bool {
    false
}

/// Whether this session is X11's, where hotkeys can be grabbed.
pub(super) fn available() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_some()
}

/// The grabbed hotkeys and the thread that reports their presses.
pub(super) struct Hotkeys {
    shared: Arc<Shared>,
}

struct Shared {
    conn: RustConnection,
    root: Window,
    /// A hidden window of the connection's own: a message to it stops the
    /// thread.
    wake: Window,
    state: Mutex<State>,
}

struct State {
    keyboard: Keyboard,
    /// The hotkeys asked for, kept to grab again when the keyboard changes.
    wanted: Vec<(u32, Hotkey)>,
    grabbed: Vec<Grab>,
    /// The key of the hotkey held down: its repeats are not presses.
    held: Option<Keycode>,
}

/// One hotkey's grab: its key and modifiers, without Caps Lock or Num Lock.
struct Grab {
    id: u32,
    keycode: Keycode,
    modifiers: u16,
}

impl Hotkeys {
    /// Connect and start the thread that sends each press to `events`.
    pub(super) fn start(events: UnboundedSender<PlatformEvent>) -> Result<Self> {
        let (conn, screen) = RustConnection::connect(None).context("connecting to X11")?;
        let root = conn.setup().roots[screen].root;
        // Held keys repeat as presses alone, not as releases and presses,
        // so a held hotkey fires once. Without XKB, it repeats.
        let detectable = conn
            .xkb_use_extension(1, 0)
            .ok()
            .and_then(|c| c.reply().ok())
            .filter(|reply| reply.supported)
            .and_then(|_| {
                let flag = PerClientFlag::DETECTABLE_AUTO_REPEAT;
                let none = xkb::BoolCtrl::from(0u32);
                conn.xkb_per_client_flags(
                    xkb::ID::USE_CORE_KBD.into(),
                    flag,
                    flag,
                    none,
                    none,
                    none,
                )
                .ok()?
                .reply()
                .ok()
            });
        if detectable.is_none() {
            log::warn!("held hotkeys will repeat: no detectable auto-repeat");
        }
        let keyboard = Keyboard::read(&conn)?;
        let wake = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            wake,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            COPY_FROM_PARENT,
            &CreateWindowAux::new(),
        )?
        .check()
        .context("making the hotkeys' window")?;
        let shared = Arc::new(Shared {
            conn,
            root,
            wake,
            state: Mutex::new(State {
                keyboard,
                wanted: Vec::new(),
                grabbed: Vec::new(),
                held: None,
            }),
        });
        let listening = shared.clone();
        std::thread::Builder::new()
            .name("shuttercrab-hotkeys".into())
            .spawn(move || listening.listen(&events))?;
        Ok(Self { shared })
    }

    /// Grab `hotkeys` in place of the ones grabbed now. Returns those that
    /// could not be grabbed.
    pub(super) fn set(&self, hotkeys: Vec<(u32, Hotkey)>) -> Vec<HotkeyConflict> {
        let mut state = self.shared.lock();
        state.wanted = hotkeys;
        self.shared.grab(&mut state)
    }
}

/// Let go of the hotkeys, and stop the thread; the connection closes with
/// it.
impl Drop for Hotkeys {
    fn drop(&mut self) {
        self.set(Vec::new());
        let wake = self.shared.wake;
        let stop = ClientMessageEvent::new(32, wake, AtomEnum::NONE, [0u32; 5]);
        // With no event mask, it goes to the window's own connection.
        let _ = self
            .shared
            .conn
            .send_event(false, wake, EventMask::NO_EVENT, stop);
        let _ = self.shared.conn.flush();
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Let go of the grabs and grab `state.wanted`. The requests go out
    /// together and their answers are read after, in one round trip.
    fn grab(&self, state: &mut State) -> Vec<HotkeyConflict> {
        for grab in state.grabbed.drain(..) {
            for extra in state.keyboard.locks() {
                let _ = self.conn.ungrab_key(
                    grab.keycode,
                    self.root,
                    ModMask::from(grab.modifiers | extra),
                );
            }
        }
        let mut conflicts = Vec::new();
        let mut asked = Vec::new();
        for &(id, hotkey) in &state.wanted {
            let Some(keycode) = state.keyboard.keycode(hotkey.key) else {
                conflicts.push(HotkeyConflict {
                    id,
                    hotkey,
                    reason: "the key is not on this keyboard".into(),
                });
                continue;
            };
            let modifiers = state.keyboard.modifiers(&hotkey);
            let cookies: Vec<_> = state
                .keyboard
                .locks()
                .into_iter()
                .filter_map(|extra| {
                    self.conn
                        .grab_key(
                            false,
                            self.root,
                            ModMask::from(modifiers | extra),
                            keycode,
                            GrabMode::ASYNC,
                            GrabMode::ASYNC,
                        )
                        .ok()
                })
                .collect();
            asked.push((id, hotkey, keycode, modifiers, cookies));
        }
        for (id, hotkey, keycode, modifiers, cookies) in asked {
            let mut taken = None;
            for cookie in cookies {
                match cookie.check() {
                    Ok(()) => {}
                    Err(ReplyError::X11Error(e)) if e.error_kind == ErrorKind::Access => {
                        taken = Some("another application has it".to_string());
                    }
                    Err(e) => taken = Some(e.to_string()),
                }
            }
            let grab = Grab {
                id,
                keycode,
                modifiers,
            };
            match taken {
                // Lets go of whichever lock combinations it did get.
                Some(reason) => {
                    for extra in state.keyboard.locks() {
                        let _ = self.conn.ungrab_key(
                            keycode,
                            self.root,
                            ModMask::from(modifiers | extra),
                        );
                    }
                    conflicts.push(HotkeyConflict { id, hotkey, reason });
                }
                None => state.grabbed.push(grab),
            }
        }
        let _ = self.conn.flush();
        conflicts
    }

    /// Wait for presses and send each hotkey's id to `events`, until the
    /// connection or the receiver goes.
    fn listen(&self, events: &UnboundedSender<PlatformEvent>) {
        loop {
            let event = match self.conn.wait_for_event() {
                Ok(event) => event,
                Err(e) => {
                    log::warn!("hotkeys stopped: {e}");
                    return;
                }
            };
            match event {
                Event::KeyPress(press) => {
                    let id = self.pressed(press.detail, u16::from(press.state));
                    if let Some(id) = id
                        && events.unbounded_send(PlatformEvent::Hotkey(id)).is_err()
                    {
                        return;
                    }
                }
                Event::KeyRelease(release) => {
                    let mut state = self.lock();
                    if state.held == Some(release.detail) {
                        state.held = None;
                    }
                }
                // Another layout or keyboard: its keys may sit elsewhere.
                Event::MappingNotify(change) if change.request != Mapping::POINTER => {
                    let mut state = self.lock();
                    match Keyboard::read(&self.conn) {
                        Ok(keyboard) => {
                            // The old grabs are let go with the old locks.
                            let old = std::mem::replace(&mut state.keyboard, keyboard);
                            let grabbed = std::mem::take(&mut state.grabbed);
                            for grab in grabbed {
                                for extra in old.locks() {
                                    let _ = self.conn.ungrab_key(
                                        grab.keycode,
                                        self.root,
                                        ModMask::from(grab.modifiers | extra),
                                    );
                                }
                            }
                            for conflict in self.grab(&mut state) {
                                log::warn!(
                                    "{} is taken by another application ({})",
                                    conflict.hotkey,
                                    conflict.reason
                                );
                            }
                        }
                        Err(e) => log::warn!("could not read the new keyboard: {e:#}"),
                    }
                }
                Event::ClientMessage(message) if message.window == self.wake => return,
                _ => {}
            }
        }
    }

    /// The hotkey `keycode` with `state`'s modifiers presses, unless it is
    /// held down and repeating.
    fn pressed(&self, keycode: Keycode, state: u16) -> Option<u32> {
        let mut locked = self.lock();
        if locked.held == Some(keycode) {
            return None;
        }
        let modifiers = state & locked.keyboard.relevant();
        let id = locked
            .grabbed
            .iter()
            .find(|g| g.keycode == keycode && g.modifiers == modifiers)?
            .id;
        locked.held = Some(keycode);
        Some(id)
    }
}

const SHIFT: u16 = 1;
const CAPS_LOCK: u16 = 1 << 1;
const CONTROL: u16 = 1 << 2;

/// The keyboard's layout as far as hotkeys need it: which key types which
/// symbol, and which modifier bits are Alt, Super and Num Lock.
struct Keyboard {
    min_keycode: Keycode,
    per_keycode: usize,
    keysyms: Vec<u32>,
    alt: u16,
    super_: u16,
    num_lock: u16,
}

impl Keyboard {
    fn read(conn: &RustConnection) -> Result<Self> {
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let mapping = conn
            .get_keyboard_mapping(min, max - min + 1)?
            .reply()
            .context("reading the keyboard")?;
        let modifiers = conn
            .get_modifier_mapping()?
            .reply()
            .context("reading the modifiers")?;
        let mut keyboard = Keyboard {
            min_keycode: min,
            per_keycode: usize::from(mapping.keysyms_per_keycode),
            keysyms: mapping.keysyms,
            alt: 0,
            super_: 0,
            num_lock: 0,
        };
        let per_modifier = usize::from(modifiers.keycodes_per_modifier()).max(1);
        let bit = |keysyms: &[u32]| {
            modifiers
                .keycodes
                .chunks(per_modifier)
                .position(|keycodes| {
                    keycodes
                        .iter()
                        .any(|&k| keysyms.iter().any(|&s| keyboard.types(k, s)))
                })
                .map_or(0, |index| 1u16 << index)
        };
        let (alt, super_, num_lock) = (
            bit(&[ALT_L, ALT_R, META_L]),
            bit(&[SUPER_L, SUPER_R]),
            bit(&[NUM_LOCK]),
        );
        // The usual places when the server names none.
        keyboard.alt = if alt == 0 { 1 << 3 } else { alt };
        keyboard.super_ = if super_ == 0 { 1 << 6 } else { super_ };
        keyboard.num_lock = if num_lock == 0 { 1 << 4 } else { num_lock };
        Ok(keyboard)
    }

    /// Whether `keycode` types `keysym`, at any level or group.
    fn types(&self, keycode: Keycode, keysym: u32) -> bool {
        let Some(row) = usize::from(keycode).checked_sub(usize::from(self.min_keycode)) else {
            return false;
        };
        self.keysyms
            .get(row * self.per_keycode..(row + 1) * self.per_keycode)
            .is_some_and(|syms| syms.contains(&keysym))
    }

    /// The first key that types `key`.
    fn keycode(&self, key: Key) -> Option<Keycode> {
        let wanted = keysyms(key);
        self.keysyms
            .chunks(self.per_keycode.max(1))
            .position(|syms| syms.iter().any(|s| wanted.contains(s)))
            .and_then(|row| Keycode::try_from(row + usize::from(self.min_keycode)).ok())
    }

    /// The modifier bits `hotkey` holds down.
    fn modifiers(&self, hotkey: &Hotkey) -> u16 {
        [
            (hotkey.shift, SHIFT),
            (hotkey.ctrl, CONTROL),
            (hotkey.alt, self.alt),
            (hotkey.win, self.super_),
        ]
        .into_iter()
        .filter(|&(on, _)| on)
        .fold(0, |bits, (_, bit)| bits | bit)
    }

    /// The modifiers a hotkey is told apart by.
    fn relevant(&self) -> u16 {
        SHIFT | CONTROL | self.alt | self.super_
    }

    /// Caps Lock and Num Lock, on or off: a hotkey is grabbed with each mix
    /// so it works whatever they are.
    fn locks(&self) -> [u16; 4] {
        [0, CAPS_LOCK, self.num_lock, CAPS_LOCK | self.num_lock]
    }
}

const ALT_L: u32 = 0xffe9;
const ALT_R: u32 = 0xffea;
const META_L: u32 = 0xffe7;
const SUPER_L: u32 = 0xffeb;
const SUPER_R: u32 = 0xffec;
const NUM_LOCK: u32 = 0xff7f;

/// The X11 keysyms that name `key`: a letter in either case.
fn keysyms(key: Key) -> Vec<u32> {
    match key {
        Key::Letter(c) => vec![u32::from(c.to_ascii_lowercase()), u32::from(c)],
        Key::Digit(c) => vec![u32::from(c)],
        // F1 to F35 follow one another.
        Key::Function(n) => vec![0xffbe + u32::from(n) - 1],
        Key::PrintScreen => vec![0xff61],
        Key::Space => vec![0x20],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A keyboard of keycodes 8 on, two keysyms each: `a`/`A` on 38, `1` on
    /// 10, F1 on 67, Print on 107, Space on 65.
    fn keyboard() -> Keyboard {
        let mut keysyms = vec![0; 2 * 120];
        let mut put = |keycode: usize, syms: [u32; 2]| {
            keysyms[(keycode - 8) * 2..(keycode - 8) * 2 + 2].copy_from_slice(&syms);
        };
        put(38, [0x61, 0x41]);
        put(10, [0x31, 0x21]);
        put(67, [0xffbe, 0xffbe]);
        put(107, [0xff61, 0xff15]);
        put(65, [0x20, 0x20]);
        Keyboard {
            min_keycode: 8,
            per_keycode: 2,
            keysyms,
            alt: 1 << 3,
            super_: 1 << 6,
            num_lock: 1 << 4,
        }
    }

    #[test]
    fn keys_are_found_on_the_keyboard() {
        let keyboard = keyboard();
        assert_eq!(keyboard.keycode(Key::Letter('A')), Some(38));
        assert_eq!(keyboard.keycode(Key::Digit('1')), Some(10));
        assert_eq!(keyboard.keycode(Key::Function(1)), Some(67));
        assert_eq!(keyboard.keycode(Key::PrintScreen), Some(107));
        assert_eq!(keyboard.keycode(Key::Space), Some(65));
        assert_eq!(keyboard.keycode(Key::Letter('Q')), None);
        assert!(keyboard.types(107, 0xff15));
        assert!(!keyboard.types(7, 0x61));
    }

    #[test]
    fn modifiers_map_to_their_bits() {
        let keyboard = keyboard();
        let bits = |text| keyboard.modifiers(&Hotkey::parse(text).unwrap());
        assert_eq!(bits("Ctrl+Alt+S"), CONTROL | 1 << 3);
        assert_eq!(bits("Shift+Win+F1"), SHIFT | 1 << 6);
        assert_eq!(bits("PrintScreen"), 0);
        assert_eq!(keyboard.locks(), [0, 2, 16, 18]);
    }
}
