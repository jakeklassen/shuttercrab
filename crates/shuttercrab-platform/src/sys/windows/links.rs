//! A URL scheme of the user's own (`HKCU\Software\Classes\SCHEME`), which
//! Windows opens by running a command with the link.

use anyhow::{Context, Result};
use windows::{
    Win32::{
        Foundation::ERROR_FILE_NOT_FOUND,
        System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegDeleteTreeW, RegSetKeyValueW},
    },
    core::HSTRING,
};

fn set(key: &str, name: Option<&str>, value: &str) -> Result<()> {
    let wide: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let name = name.map(HSTRING::from);
    // SAFETY: `wide` is a NUL-terminated string of the size given.
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            name.as_ref().unwrap_or(&HSTRING::new()),
            REG_SZ.0,
            Some(wide.as_ptr().cast()),
            (wide.len() * 2) as u32,
        )
    }
    .ok()
    .with_context(|| format!("could not write {key}"))
}

/// Open `scheme:` links by running this program with `flag` and the link,
/// named `description` where Windows asks which app to use.
pub fn register(scheme: &str, flag: &str, description: &str) -> Result<()> {
    let exe = std::env::current_exe().context("no path for this executable")?;
    let key = format!(r"Software\Classes\{scheme}");
    set(&key, None, &format!("URL:{description}"))?;
    set(&key, Some("URL Protocol"), "")?;
    set(
        &format!(r"{key}\DefaultIcon"),
        None,
        &format!("\"{}\",0", exe.display()),
    )?;
    set(
        &format!(r"{key}\shell\open\command"),
        None,
        &format!("\"{}\" {flag} \"%1\"", exe.display()),
    )
}

/// Stop opening `scheme:` links.
pub fn unregister(scheme: &str) -> Result<()> {
    let key = HSTRING::from(format!(r"Software\Classes\{scheme}"));
    // SAFETY: a plain call with a NUL-terminated name.
    let deleted = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &key) };
    if deleted.is_ok() || deleted == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        deleted.ok().context("could not remove the link scheme")
    }
}
