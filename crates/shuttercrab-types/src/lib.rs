//! The identities Shuttercrab's crates pass between them: a window and a
//! monitor on screen, the same type on every OS. Only an OS backend makes
//! one from its own handle or turns one back; the app only compares them
//! and passes them on. They name what is on screen now, so they are never
//! saved.

/// A window on screen, as the OS knows it: on Windows, its `HWND`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowId(u64);

impl WindowId {
    /// The window an OS backend knows by `raw`.
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The OS's own value, for its backend and for passing to the recorder
    /// process.
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// A monitor, as the OS knows it: on Windows, its `HMONITOR`, which GPUI
/// also uses as its display id there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MonitorId(u64);

impl MonitorId {
    /// The monitor an OS backend knows by `raw`.
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The OS's own value, for its backend and for passing to the recorder
    /// process.
    pub fn raw(self) -> u64 {
        self.0
    }
}
