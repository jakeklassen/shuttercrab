//! The OS backends: what only an OS can do, behind the crate's public
//! modules. `imp` is the one this build uses. The modules are `pub` so the
//! public modules can re-export from them; `sys` itself is private.

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use self::windows as imp;
