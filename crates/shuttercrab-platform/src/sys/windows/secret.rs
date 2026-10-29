//! Secrets encrypted to the signed-in user with DPAPI
//! (`CryptProtectData`), as hex for the settings file.

use crate::secret::{from_hex, to_hex};
use anyhow::{Context, Result};
use windows::Win32::{
    Foundation::{HLOCAL, LocalFree},
    Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    },
};

/// Mixed in, so only Shuttercrab reads back what it kept.
const ENTROPY: &[u8] = b"Shuttercrab secret";

fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    }
}

/// The bytes DPAPI gave back, freed.
fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    // SAFETY: DPAPI allocated `cbData` bytes at `pbData` with LocalAlloc.
    unsafe {
        let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
        bytes
    }
}

/// `secret`, encrypted to this user, as hex.
pub fn protect(secret: &str) -> Result<String> {
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: the blobs point at live bytes for the call; DPAPI only reads
    // them, and allocates `out`.
    unsafe {
        CryptProtectData(
            &blob(secret.as_bytes()),
            None,
            Some(&blob(ENTROPY)),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    }
    .context("could not encrypt the secret")?;
    Ok(to_hex(&take(out)))
}

/// What [`protect`] kept as `kept`.
pub fn unprotect(kept: &str) -> Result<String> {
    let bytes = from_hex(kept).context("the kept secret is not hex")?;
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in `protect`.
    unsafe {
        CryptUnprotectData(
            &blob(&bytes),
            None,
            Some(&blob(ENTROPY)),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    }
    .context("could not decrypt the secret: it was kept by another user or PC")?;
    String::from_utf8(take(out)).context("the secret is not text")
}
