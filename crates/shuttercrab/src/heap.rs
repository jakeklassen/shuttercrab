//! How much of Shuttercrab's memory is its own Rust heap, as opposed to
//! Windows, the graphics driver, Media Foundation and other native code,
//! and a log line that says so.
//!
//! [`Counting`] wraps the system allocator and keeps a running total; the
//! app sets it as the global allocator in `main`. It costs one atomic add
//! per allocation.

use gpui_kit::AsyncApp;
use shuttercrab_platform::memory::{Usage, mb};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

/// Bytes currently allocated through [`Counting`].
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

/// The system allocator, counting what is allocated.
pub struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
        ALLOCATED.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, layout, size) };
        if !q.is_null() {
            ALLOCATED.fetch_sub(layout.size(), Ordering::Relaxed);
            ALLOCATED.fetch_add(size, Ordering::Relaxed);
        }
        q
    }
}

/// Bytes on the Rust heap now; 0 unless [`Counting`] is the allocator.
pub fn allocated() -> u64 {
    ALLOCATED.load(Ordering::Relaxed) as u64
}

/// Log the process's memory and how much of it is the Rust heap.
pub fn log_memory(when: &str) {
    log::info!(
        "memory {when}: {}, Rust heap {}",
        Usage::now(),
        mb(allocated())
    );
}

/// How long after an event its memory is logged: long enough for threads
/// and windows that end with it to be gone.
const SETTLE: Duration = Duration::from_secs(3);

/// Log the memory a moment after `when`, once it has settled.
pub fn log_memory_soon(when: &'static str, cx: &mut AsyncApp) {
    cx.spawn(async move |cx| {
        cx.background_executor().timer(SETTLE).await;
        log_memory(when);
    })
    .detach();
}
