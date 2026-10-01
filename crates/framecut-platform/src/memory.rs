//! How much memory Framecut uses, for the log (PRD §23) and for finding
//! where it goes.

use windows::{
    Win32::{
        Graphics::Dxgi::{
            CreateDXGIFactory1, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO,
            IDXGIAdapter3, IDXGIFactory1,
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

/// This process's memory, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Committed memory only this process can use: Task Manager's figure.
    pub private: u64,
    /// The graphics adapters' own memory this process uses.
    pub graphics: u64,
}

impl Usage {
    /// The usage now.
    pub fn now() -> Self {
        Self {
            private: private(),
            graphics: graphics(),
        }
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

/// The sum over adapters of this process's use of their local memory.
fn graphics() -> u64 {
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return 0;
    };
    let mut total = 0;
    let mut index = 0;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(index) } {
        let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        if let Ok(adapter) = adapter.cast::<IDXGIAdapter3>()
            && unsafe {
                adapter.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info)
            }
            .is_ok()
        {
            total += info.CurrentUsage;
        }
        index += 1;
    }
    total
}

/// Bytes as megabytes, one decimal.
pub fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "private {}, graphics {}",
            mb(self.private),
            mb(self.graphics)
        )
    }
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
