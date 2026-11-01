//! One Shuttercrab per user session.

use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE},
        System::Threading::CreateMutexW,
    },
    core::HSTRING,
};

/// Held while this process is the running instance.
pub struct SingleInstance(HANDLE);

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: the mutex handle is owned by this value alone (the field
        // is private) and closed only here.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Claim `name` for this process. `None` when another process already holds
/// it. The name is per session (`Local\`), so other users are unaffected.
pub fn single_instance(name: &str) -> Option<SingleInstance> {
    let name = HSTRING::from(format!("Local\\{name}"));
    // SAFETY: `name` is a null-terminated HSTRING that lives through the
    // call; no security attributes are passed.
    let handle = unsafe { CreateMutexW(None, false, &name) }.ok()?;
    // SAFETY: takes no arguments; it reads this thread's last error.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // SAFETY: `handle` was just opened and is not used after this.
        let _ = unsafe { CloseHandle(handle) };
        return None;
    }
    Some(SingleInstance(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_claim_fails_until_the_first_is_dropped() {
        let name = format!("ShuttercrabTest.{}", std::process::id());
        let first = single_instance(&name).unwrap();
        assert!(single_instance(&name).is_none());
        drop(first);
        assert!(single_instance(&name).is_some());
    }
}
