//! This process's memory on Windows: its private commit, and the graphics
//! adapters' memory it uses (DXGI's video memory budget).

use crate::memory::Usage;
use windows::{
    Win32::{
        Graphics::Dxgi::{
            CreateDXGIFactory1, DXGI_MEMORY_SEGMENT_GROUP_LOCAL,
            DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter3,
            IDXGIFactory1,
        },
        System::{
            ProcessStatus::{
                GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
            },
            Threading::GetCurrentProcess,
        },
    },
    core::Interface,
};
/// The usage now.
pub(crate) fn usage() -> Usage {
    let (graphics, graphics_shared) = graphics();
    Usage {
        private: private(),
        graphics,
        graphics_shared,
    }
}

fn private() -> u64 {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let read = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
    };
    read.map_or(0, |()| counters.PrivateUsage as u64)
}

/// The sum over adapters of this process's use of their local memory, and of
/// their non-local (system) memory.
fn graphics() -> (u64, u64) {
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return (0, 0);
    };
    let (mut local, mut shared) = (0, 0);
    let mut index = 0;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(index) } {
        if let Ok(adapter) = adapter.cast::<IDXGIAdapter3>() {
            let usage = |group| {
                let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                unsafe { adapter.QueryVideoMemoryInfo(0, group, &mut info) }
                    .map_or(0, |()| info.CurrentUsage)
            };
            local += usage(DXGI_MEMORY_SEGMENT_GROUP_LOCAL);
            shared += usage(DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL);
        }
        index += 1;
    }
    (local, shared)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_running_process_has_private_memory() {
        let usage = Usage::now();
        assert!(usage.private > 1024 * 1024, "{usage}");
        assert!(usage.to_string().starts_with("private "));
    }
}
