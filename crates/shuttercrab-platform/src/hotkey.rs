//! Parsing hotkeys such as `Ctrl+Alt+S` into Win32 modifiers and virtual keys.

use anyhow::{Result, bail};
use std::fmt;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, VK_SNAPSHOT,
    VK_SPACE,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// Win32 virtual-key code.
    pub key: u32,
}

impl Hotkey {
    /// Parse `Ctrl+Alt+S`, `Win+Shift+F1`, `Alt+PrintScreen` and the like.
    /// Case and spaces around `+` do not matter; at least one modifier is
    /// required so the hotkey cannot swallow ordinary typing, except for
    /// Print Screen, which types nothing.
    pub fn parse(text: &str) -> Result<Self> {
        let mut hotkey = Hotkey {
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            key: 0,
        };
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let Some((key, modifiers)) = parts.split_last() else {
            bail!("empty hotkey");
        };
        for modifier in modifiers {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => hotkey.ctrl = true,
                "alt" => hotkey.alt = true,
                "shift" => hotkey.shift = true,
                "win" | "super" | "meta" => hotkey.win = true,
                other => bail!("unknown modifier {other:?} in {text:?}"),
            }
        }
        hotkey.key =
            virtual_key(key).ok_or_else(|| anyhow::anyhow!("unknown key {key:?} in {text:?}"))?;
        if !(hotkey.ctrl || hotkey.alt || hotkey.win || hotkey.key == VK_SNAPSHOT.0 as u32) {
            bail!("{text:?} needs Ctrl, Alt or Win");
        }
        Ok(hotkey)
    }

    pub(crate) fn modifiers(&self) -> HOT_KEY_MODIFIERS {
        let mut m = MOD_NOREPEAT;
        for (on, flag) in [
            (self.ctrl, MOD_CONTROL),
            (self.alt, MOD_ALT),
            (self.shift, MOD_SHIFT),
            (self.win, MOD_WIN),
        ] {
            if on {
                m |= flag;
            }
        }
        m
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.ctrl, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
            (self.win, "Win+"),
        ] {
            if on {
                f.write_str(name)?;
            }
        }
        match self.key {
            k if (0x30..=0x39).contains(&k) || (0x41..=0x5A).contains(&k) => {
                write!(f, "{}", k as u8 as char)
            }
            k if (0x70..=0x87).contains(&k) => write!(f, "F{}", k - 0x6F),
            k if k == VK_SNAPSHOT.0 as u32 => f.write_str("PrintScreen"),
            k if k == VK_SPACE.0 as u32 => f.write_str("Space"),
            k => write!(f, "VK{k:#04X}"),
        }
    }
}

/// Whether Windows keeps Print Screen for its own Snipping Tool ("Use the
/// Print screen key to open screen capture"). While it does, a Print
/// Screen hotkey registers but never fires. Windows 11 has it on until the
/// user turns it off, which is when the value is first written.
pub fn windows_takes_print_screen() -> bool {
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

fn virtual_key(name: &str) -> Option<u32> {
    let upper = name.to_ascii_uppercase();
    if upper.len() == 1 {
        let c = upper.as_bytes()[0];
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(c as u32);
        }
    }
    if let Some(n) = upper.strip_prefix('F').and_then(|n| n.parse::<u32>().ok())
        && (1..=24).contains(&n)
    {
        return Some(0x6F + n);
    }
    match upper.as_str() {
        "PRINTSCREEN" | "PRTSC" | "PRTSCN" => Some(VK_SNAPSHOT.0 as u32),
        "SPACE" => Some(VK_SPACE.0 as u32),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_hotkeys() {
        let h = Hotkey::parse("Ctrl+Alt+S").unwrap();
        assert!(h.ctrl && h.alt && !h.shift && !h.win);
        assert_eq!(h.key, b'S' as u32);
        assert_eq!(h.modifiers(), MOD_NOREPEAT | MOD_CONTROL | MOD_ALT);
        assert_eq!(Hotkey::parse("win + shift + f12").unwrap().key, 0x7B);
        assert_eq!(
            Hotkey::parse("Alt+PrintScreen").unwrap().key,
            VK_SNAPSHOT.0 as u32
        );
        assert_eq!(
            Hotkey::parse("PrintScreen").unwrap().to_string(),
            "PrintScreen"
        );
        assert_eq!(
            Hotkey::parse("Shift+PrtScn").unwrap().to_string(),
            "Shift+PrintScreen"
        );
        assert_eq!(
            Hotkey::parse("Ctrl+Alt+Shift+Q").unwrap().to_string(),
            "Ctrl+Alt+Shift+Q"
        );
        assert_eq!(
            Hotkey::parse("ctrl+alt+s").unwrap().to_string(),
            "Ctrl+Alt+S"
        );
    }

    #[test]
    fn rejects_bad_hotkeys() {
        for text in [
            "",
            "S",
            "Shift+S",
            "Ctrl+Hyper+S",
            "Ctrl+",
            "Ctrl+F25",
            "Ctrl+Alt+SS",
        ] {
            assert!(Hotkey::parse(text).is_err(), "{text:?}");
        }
    }
}
