//! Launching Shuttercrab when the user signs in (PRD §26): the per-user `Run`
//! key in the registry, which is also what Task Manager's Startup apps page
//! shows and switches.

use anyhow::{Context, Result};
use windows::{
    Win32::{
        Foundation::ERROR_FILE_NOT_FOUND,
        System::Registry::{
            HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW,
            RegSetKeyValueW,
        },
    },
    core::HSTRING,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const NAME: &str = "Shuttercrab";

/// Whether Shuttercrab starts when the user signs in.
pub fn launch_at_startup() -> bool {
    entry(NAME).is_some()
}

/// Start (or stop starting) this executable when the user signs in.
pub fn set_launch_at_startup(enabled: bool) -> Result<()> {
    if enabled {
        let exe = std::env::current_exe().context("no path for this executable")?;
        set_entry(NAME, &format!("\"{}\"", exe.display()))
    } else {
        remove_entry(NAME)
    }
}

fn entry(name: &str) -> Option<String> {
    let mut size = 0u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_KEY),
            &HSTRING::from(name),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
        .ok()
        .ok()?;
        let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_KEY),
            &HSTRING::from(name),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
        .ok()
        .ok()?;
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..len]))
    }
}

fn set_entry(name: &str, command: &str) -> Result<()> {
    let data: Vec<u16> = command.encode_utf16().chain([0]).collect();
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_KEY),
            &HSTRING::from(name),
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        )
        .ok()
        .context("could not write the startup entry")
    }
}

fn remove_entry(name: &str) -> Result<()> {
    let result = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_KEY),
            &HSTRING::from(name),
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    result.ok().context("could not remove the startup entry")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_round_trip_without_touching_shuttercrabs_own() {
        let name = format!("ShuttercrabTest.{}", std::process::id());
        assert_eq!(entry(&name), None);
        set_entry(&name, r#""C:\Program Files\Shuttercrab\shuttercrab.exe""#).unwrap();
        assert_eq!(
            entry(&name).as_deref(),
            Some(r#""C:\Program Files\Shuttercrab\shuttercrab.exe""#)
        );
        remove_entry(&name).unwrap();
        assert_eq!(entry(&name), None);
        // Removing twice is fine.
        remove_entry(&name).unwrap();
    }
}
