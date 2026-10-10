//! The OS backends: what only an OS can do, behind the crate's public
//! modules. `imp` is the one this build uses: Windows', Linux's, or on an
//! OS with no backend yet, one that says so. The modules are `pub` so the
//! public modules can re-export from them; `sys` itself is private.

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use self::windows as imp;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use self::linux as imp;

#[cfg(not(windows))]
pub mod unsupported;
#[cfg(not(any(windows, target_os = "linux")))]
pub use self::unsupported as imp;
