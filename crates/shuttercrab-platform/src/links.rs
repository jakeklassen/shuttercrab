//! Links that open Shuttercrab, such as `shuttercrab://watch/…` for a live
//! stream: clicking one anywhere runs this program with it.

pub use crate::sys::imp::links::{register, unregister};
