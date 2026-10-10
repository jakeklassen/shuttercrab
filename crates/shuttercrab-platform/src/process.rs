//! Starting a helper process, with its standard input, output and error as
//! pipes, and without the OS's busy pointer.

use std::fs::File;

pub use crate::sys::imp::process::{Process, spawn_quiet};

/// A started process and the parent's ends of its pipes.
pub struct Spawned {
    pub process: Process,
    /// Its standard input: what is written here, it reads.
    pub stdin: File,
    /// Its standard output and error, to read.
    pub stdout: File,
    pub stderr: File,
}
