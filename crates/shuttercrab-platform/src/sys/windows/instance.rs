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
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Claim `name` for this process. `None` when another process already holds
/// it. The name is per session (`Local\`), so other users are unaffected.
pub fn single_instance(name: &str) -> Option<SingleInstance> {
    let name = HSTRING::from(format!("Local\\{name}"));
    let handle = unsafe { CreateMutexW(None, false, &name) }.ok()?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
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
