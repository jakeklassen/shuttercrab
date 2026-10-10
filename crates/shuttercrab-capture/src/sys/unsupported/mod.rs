//! The backend for an OS Shuttercrab does not support yet: everything is
//! there, so the app builds and starts, and every capture, recording and
//! playback fails with an error saying it is not supported here.

pub mod play;
pub mod record;
#[cfg(not(target_os = "linux"))]
pub mod service;

/// The error for `what`, which this OS cannot do yet.
fn unsupported(what: &str) -> anyhow::Error {
    anyhow::anyhow!("{what} is not supported on this OS yet")
}
