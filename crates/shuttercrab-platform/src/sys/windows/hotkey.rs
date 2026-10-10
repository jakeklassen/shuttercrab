//! Hotkeys on Windows: Win32 modifiers and virtual keys for
//! `RegisterHotKey`, and whether Windows keeps Print Screen for itself.

use crate::hotkey::{Hotkey, Key};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, VK_F1, VK_SNAPSHOT,
    VK_SPACE,
};

/// The hotkey's modifiers, as `RegisterHotKey` takes them.
pub(crate) fn modifiers(hotkey: &Hotkey) -> HOT_KEY_MODIFIERS {
    let mut m = MOD_NOREPEAT;
    for (on, flag) in [
        (hotkey.ctrl, MOD_CONTROL),
        (hotkey.alt, MOD_ALT),
        (hotkey.shift, MOD_SHIFT),
        (hotkey.win, MOD_WIN),
    ] {
        if on {
            m |= flag;
        }
    }
    m
}

/// The key's virtual-key code. Letters and digits are their own ASCII
/// codes; F1 to F24 follow one another.
pub(crate) fn virtual_key(key: Key) -> u32 {
    match key {
        Key::Letter(c) | Key::Digit(c) => c as u32,
        Key::Function(n) => VK_F1.0 as u32 + u32::from(n) - 1,
        Key::PrintScreen => VK_SNAPSHOT.0 as u32,
        Key::Space => VK_SPACE.0 as u32,
    }
}

/// Whether Windows keeps Print Screen for its own Snipping Tool ("Use the
/// Print screen key to open screen capture"). While it does, a Print
/// Screen hotkey registers but never fires. Windows 11 has it on until the
/// user turns it off, which is when the value is first written.
pub fn print_screen_taken() -> bool {
    use windows::{
        Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
        core::w,
    };
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Control Panel\Keyboard"),
            w!("PrintScreenKeyForSnippingEnabled"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&mut size),
        )
    };
    read.is_err() || value != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_their_virtual_keys() {
        let h = Hotkey::parse("Ctrl+Alt+S").unwrap();
        assert_eq!(modifiers(&h), MOD_NOREPEAT | MOD_CONTROL | MOD_ALT);
        assert_eq!(virtual_key(h.key), b'S' as u32);
        assert_eq!(virtual_key(Key::Digit('0')), 0x30);
        assert_eq!(virtual_key(Key::Function(1)), 0x70);
        assert_eq!(virtual_key(Key::Function(12)), 0x7B);
        assert_eq!(virtual_key(Key::Function(24)), 0x87);
        assert_eq!(virtual_key(Key::PrintScreen), 0x2C);
        assert_eq!(virtual_key(Key::Space), 0x20);
    }
}
