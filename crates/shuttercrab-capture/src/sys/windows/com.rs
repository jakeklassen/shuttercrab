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
            // SAFETY: the reserved argument is `None`, as required. A
            // success, `S_FALSE` included, is matched by one
            // `CoUninitialize` in `drop`, on this same thread: the
            // `PhantomData` keeps the value from moving to another.
            joined: unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok(),
            _thread: PhantomData,
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.joined {
            // SAFETY: this balances the successful `CoInitializeEx` in
            // `multithreaded`, once, on the thread that made it, since a
            // `ComApartment` cannot be sent.
            unsafe { CoUninitialize() };
        }
    }
}
