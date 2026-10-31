//! COM for a thread of capture's own, such as a sound thread.

use std::marker::PhantomData;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// This thread in COM's multithreaded apartment, until dropped.
pub(crate) struct ComApartment {
    /// Whether this joined it, and so leaves it again. A thread already
    /// in another apartment carries on there: what capture uses works in
    /// either.
    joined: bool,
    /// Left on the thread that joined.
    _thread: PhantomData<*const ()>,
}

impl ComApartment {
    pub(crate) fn multithreaded() -> Self {
        Self {
            joined: unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok(),
            _thread: PhantomData,
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.joined {
            unsafe { CoUninitialize() };
        }
    }
}
