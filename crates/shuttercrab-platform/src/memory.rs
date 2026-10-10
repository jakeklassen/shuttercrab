//! How much memory Shuttercrab uses, for the log (PRD §23) and for finding
//! where it goes.

/// This process's memory, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Committed memory only this process can use: Task Manager's figure.
    pub private: u64,
    /// The graphics adapters' own memory this process uses.
    pub graphics: u64,
    /// Graphics memory this process uses in system memory (the adapters'
    /// non-local segment), such as readback and shared surfaces.
    pub graphics_shared: u64,
}

impl Usage {
    /// The usage now.
    pub fn now() -> Self {
        crate::sys::imp::memory::usage()
    }
}

/// Bytes as megabytes, one decimal.
pub fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "private {}, graphics {} (+{} in system memory)",
            mb(self.private),
            mb(self.graphics),
            mb(self.graphics_shared)
        )
    }
}
