//! Hotkeys such as `Ctrl+Alt+S`: their modifiers and key, parsed from and
//! written back to the text the settings keep.

use anyhow::{Result, bail};
use std::fmt;

pub use crate::sys::imp::hotkey::print_screen_taken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key: Key,
}

/// The key a hotkey presses with its modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A letter, `A` to `Z` in capitals.
    Letter(char),
    /// A digit on the main keyboard, `0` to `9`.
    Digit(char),
    /// A function key, 1 to 24.
    Function(u8),
    PrintScreen,
    Space,
}

impl Hotkey {
    /// Parse `Ctrl+Alt+S`, `Win+Shift+F1`, `Alt+PrintScreen` and the like.
    /// Case and spaces around `+` do not matter; at least one modifier is
    /// required so the hotkey cannot swallow ordinary typing, except for
    /// Print Screen, which types nothing.
    pub fn parse(text: &str) -> Result<Self> {
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let Some((key, modifiers)) = parts.split_last() else {
            bail!("empty hotkey");
        };
        let key =
            Key::parse(key).ok_or_else(|| anyhow::anyhow!("unknown key {key:?} in {text:?}"))?;
        let mut hotkey = Hotkey {
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            key,
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
        if !(hotkey.ctrl || hotkey.alt || hotkey.win || hotkey.key == Key::PrintScreen) {
            bail!("{text:?} needs Ctrl, Alt or Win");
        }
        Ok(hotkey)
    }
}

impl Key {
    fn parse(name: &str) -> Option<Self> {
        let upper = name.to_ascii_uppercase();
        if let [c] = upper.as_bytes() {
            let c = char::from(*c);
            if c.is_ascii_uppercase() {
                return Some(Key::Letter(c));
            }
            if c.is_ascii_digit() {
                return Some(Key::Digit(c));
            }
        }
        if let Some(n) = upper.strip_prefix('F').and_then(|n| n.parse::<u8>().ok())
            && (1..=24).contains(&n)
        {
            return Some(Key::Function(n));
        }
        match upper.as_str() {
            "PRINTSCREEN" | "PRTSC" | "PRTSCN" => Some(Key::PrintScreen),
            "SPACE" => Some(Key::Space),
            _ => None,
        }
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
            Key::Letter(c) | Key::Digit(c) => write!(f, "{c}"),
            Key::Function(n) => write!(f, "F{n}"),
            Key::PrintScreen => f.write_str("PrintScreen"),
            Key::Space => f.write_str("Space"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_hotkeys() {
        let h = Hotkey::parse("Ctrl+Alt+S").unwrap();
        assert!(h.ctrl && h.alt && !h.shift && !h.win);
        assert_eq!(h.key, Key::Letter('S'));
        assert_eq!(
            Hotkey::parse("win + shift + f12").unwrap().key,
            Key::Function(12)
        );
        assert_eq!(Hotkey::parse("Ctrl+Alt+7").unwrap().key, Key::Digit('7'));
        assert_eq!(
            Hotkey::parse("Alt+PrintScreen").unwrap().key,
            Key::PrintScreen
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
        assert_eq!(
            Hotkey::parse("Win+Shift+F12").unwrap().to_string(),
            "Shift+Win+F12"
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
