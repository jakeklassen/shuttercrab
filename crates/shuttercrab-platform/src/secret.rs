//! Keeping a secret, such as a streaming host key, in the settings file
//! without writing it in the clear: encrypted to the signed-in user
//! (Windows' DPAPI), so another user, or a copy of the file on another PC,
//! cannot read it back.

pub use crate::sys::imp::secret::{protect, unprotect};

/// `bytes` as lowercase hex, for the settings file.
#[cfg(any(windows, test))]
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The bytes `hex` spells, if it is hex.
#[cfg(any(windows, test))]
pub(crate) fn from_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let bytes = [0x00, 0x7f, 0xff, 0x10];
        assert_eq!(to_hex(&bytes), "007fff10");
        assert_eq!(from_hex("007fff10").unwrap(), bytes);
        assert!(from_hex("abc").is_none());
        assert!(from_hex("zz").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn a_secret_comes_back_but_is_never_stored_in_the_clear() {
        let kept = protect("host-key-123").unwrap();
        assert!(!kept.contains("host-key-123"));
        assert!(!kept.contains(&to_hex(b"host-key-123")));
        assert_eq!(unprotect(&kept).unwrap(), "host-key-123");
        assert!(unprotect("00ff").is_err());
    }
}
