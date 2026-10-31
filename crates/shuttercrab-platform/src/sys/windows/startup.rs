//! Launching Shuttercrab when the user signs in (PRD §26): the per-user `Run`
//! key in the registry. Task Manager's Startup apps page shows its entries,
//! and switching one off there leaves it in place but marks it disabled in
//! `StartupApproved\Run`.

use crate::startup::BACKGROUND_FLAG;
use anyhow::{Context, Result};
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows_registry::CURRENT_USER;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const NAME: &str = "Shuttercrab";

/// Whether Shuttercrab starts when the user signs in: it has an entry, and
/// it is not switched off in Task Manager.
pub fn launch_at_startup() -> bool {
    entry(NAME).is_some() && approved(NAME)
}

/// Start (or stop starting) this executable when the user signs in.
pub fn set_launch_at_startup(enabled: bool) -> Result<()> {
    if enabled {
        let exe = std::env::current_exe().context("no path for this executable")?;
        set_entry(NAME, &format!("\"{}\" {BACKGROUND_FLAG}", exe.display()))?;
    } else {
        remove_entry(NAME)?;
    }
    // Either way, forget a switch made in Task Manager: on means on.
    forget_approval(NAME)
}

/// Add [`BACKGROUND_FLAG`] to a startup entry written without it (by 0.1.1
/// and earlier), keeping the program it starts. Returns whether it did.
pub fn add_background_flag() -> Result<bool> {
    match entry(NAME) {
        Some(command) if !command.ends_with(BACKGROUND_FLAG) => {
            set_entry(NAME, &with_background_flag(&command))?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn with_background_flag(command: &str) -> String {
    format!("{} {BACKGROUND_FLAG}", command.trim_end())
}

fn entry(name: &str) -> Option<String> {
    CURRENT_USER.open(RUN_KEY).ok()?.get_string(name).ok()
}

fn set_entry(name: &str, command: &str) -> Result<()> {
    CURRENT_USER
        .create(RUN_KEY)
        .and_then(|key| key.set_string(name, command))
        .context("could not write the startup entry")
}

fn remove_entry(name: &str) -> Result<()> {
    remove_value(RUN_KEY, name).context("could not remove the startup entry")
}

/// Whether Task Manager leaves the entry `name` on. Its mark is 12 bytes:
/// the first even when on (2, or 6) and odd when switched off, then when
/// it was switched off. With no mark, the entry is on.
fn approved(name: &str) -> bool {
    let mark = CURRENT_USER
        .open(APPROVED_KEY)
        .and_then(|key| key.get_value(name));
    match mark {
        Ok(mark) => mark.first().is_none_or(|first| first % 2 == 0),
        Err(_) => true,
    }
}

fn forget_approval(name: &str) -> Result<()> {
    remove_value(APPROVED_KEY, name).context("could not clear Task Manager's startup switch")
}

/// Remove the value `name` from the key `path`, if either is there.
fn remove_value(path: &str, name: &str) -> windows::core::Result<()> {
    let missing = |e: &windows::core::Error| e.code() == ERROR_FILE_NOT_FOUND.to_hresult();
    match CURRENT_USER.options().write().open(path) {
        Ok(key) => match key.remove_value(name) {
            Err(e) if !missing(&e) => Err(e),
            _ => Ok(()),
        },
        Err(e) if !missing(&e) => Err(e),
        Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_registry::Type;

    /// A test entry's name, removed again even when the test fails.
    struct TestName(String);

    impl TestName {
        fn new(what: &str) -> Self {
            Self(format!("ShuttercrabTest.{what}.{}", std::process::id()))
        }
    }

    impl Drop for TestName {
        fn drop(&mut self) {
            let _ = remove_entry(&self.0);
            let _ = forget_approval(&self.0);
        }
    }

    #[test]
    fn entries_round_trip_without_touching_shuttercrabs_own() {
        let test = TestName::new("entry");
        let name = test.0.as_str();
        assert_eq!(entry(name), None);
        set_entry(name, r#""C:\Program Files\Shuttercrab\shuttercrab.exe""#).unwrap();
        assert_eq!(
            entry(name).as_deref(),
            Some(r#""C:\Program Files\Shuttercrab\shuttercrab.exe""#)
        );
        remove_entry(name).unwrap();
        assert_eq!(entry(name), None);
        // Removing twice is fine.
        remove_entry(name).unwrap();
    }

    #[test]
    fn an_entry_switched_off_in_task_manager_is_off_until_switched_on() {
        let test = TestName::new("approved");
        let name = test.0.as_str();
        let approved_key = CURRENT_USER.create(APPROVED_KEY).unwrap();
        assert!(approved(name), "no mark is on");
        let mark = |first| [first, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        approved_key.set_bytes(name, Type::Bytes, &mark(3)).unwrap();
        assert!(!approved(name), "an odd mark is off");
        approved_key.set_bytes(name, Type::Bytes, &mark(2)).unwrap();
        assert!(approved(name), "an even mark is on");
        approved_key.set_bytes(name, Type::Bytes, &mark(3)).unwrap();
        forget_approval(name).unwrap();
        assert!(approved(name), "forgetting the mark switches it on");
        forget_approval(name).unwrap();
    }

    #[test]
    fn an_old_entry_gains_the_background_flag_and_keeps_its_program() {
        assert_eq!(
            with_background_flag(
                r#""C:\Users\a\AppData\Local\Shuttercrab\current\shuttercrab.exe""#
            ),
            r#""C:\Users\a\AppData\Local\Shuttercrab\current\shuttercrab.exe" --background"#
        );
    }
}
