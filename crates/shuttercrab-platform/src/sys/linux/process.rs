//! Starting a helper process on Linux, with its standard input, output and
//! error as pipes.

use crate::process::Spawned;
use anyhow::{Context, Result};
use std::{
    fs::File,
    os::fd::OwnedFd,
    path::Path,
    process::{Child, Command, Stdio},
    sync::Mutex,
};

/// A started process, to wait for.
pub struct Process(Mutex<Child>);

impl Process {
    /// Wait for the process to end; returns its exit code (1 if a signal
    /// ended it).
    pub fn wait(&self) -> Result<u32> {
        let mut child = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("the process handle was poisoned"))?;
        let status = child.wait().context("could not wait for the process")?;
        Ok(status.code().map_or(1, |code| code as u32))
    }
}

/// Start `exe` with `args`, its standard streams as pipes.
pub fn spawn_quiet(exe: &Path, args: &[&str]) -> Result<Spawned> {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not start {}", exe.display()))?;
    let pipe = |fd: Option<OwnedFd>| fd.map(File::from).context("a pipe is missing");
    let stdin = pipe(child.stdin.take().map(OwnedFd::from))?;
    let stdout = pipe(child.stdout.take().map(OwnedFd::from))?;
    let stderr = pipe(child.stderr.take().map(OwnedFd::from))?;
    Ok(Spawned {
        process: Process(Mutex::new(child)),
        stdin,
        stdout,
        stderr,
    })
}
