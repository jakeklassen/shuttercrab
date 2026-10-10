//! Starting a helper process without Windows' busy pointer.
//!
//! After a program starts, Windows shows its "app starting" pointer for a
//! few seconds, until the program shows a window. The recording helper
//! never does, so the busy pointer was recorded over the first seconds of
//! every recording. `STARTF_FORCEOFFFEEDBACK` turns it off, and Rust's
//! `Command` cannot set it yet, so the helper is started here, with its
//! standard input, output and error as pipes, as `Command` would.

use crate::process::Spawned;
use anyhow::{Context, Result, bail};
use std::{
    fs::File,
    os::windows::io::{FromRawHandle, OwnedHandle},
    path::Path,
};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0,
        },
        Security::SECURITY_ATTRIBUTES,
        System::{
            Pipes::CreatePipe,
            Threading::{
                CREATE_NO_WINDOW, CreateProcessW, DeleteProcThreadAttributeList,
                EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
                InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, STARTF_FORCEOFFFEEDBACK,
                STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
                WaitForSingleObject,
            },
        },
    },
    core::PWSTR,
};

/// A started process, to wait for.
pub struct Process(OwnedHandle);

impl Process {
    /// Wait for the process to end; returns its exit code.
    pub fn wait(&self) -> Result<u32> {
        let handle = HANDLE(std::os::windows::io::AsRawHandle::as_raw_handle(&self.0));
        unsafe {
            if WaitForSingleObject(handle, INFINITE) != WAIT_OBJECT_0 {
                bail!("could not wait for the process");
            }
            let mut code = 0;
            GetExitCodeProcess(handle, &mut code).context("no exit code")?;
            Ok(code)
        }
    }
}

/// One pipe: the end the child gets (inheritable) and the parent's end.
struct Pipe {
    child: HANDLE,
    parent: HANDLE,
}

impl Pipe {
    /// A pipe the child reads (`child_reads`) or writes.
    fn new(child_reads: bool) -> Result<Self> {
        let inherit = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: true.into(),
        };
        let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
        unsafe { CreatePipe(&mut read, &mut write, Some(&inherit), 0) }
            .context("could not make a pipe")?;
        let (child, parent) = if child_reads {
            (read, write)
        } else {
            (write, read)
        };
        // Only the child's end is inherited.
        unsafe { SetHandleInformation(parent, HANDLE_FLAG_INHERIT.0, Default::default()) }
            .context("could not keep a pipe to this process")?;
        Ok(Self { child, parent })
    }

    /// The parent's end, owned.
    fn into_parent(self) -> File {
        unsafe {
            let _ = CloseHandle(self.child);
            File::from_raw_handle(self.parent.0)
        }
    }
}

/// Start `exe` with `args`, without a console window and without the busy
/// pointer, its standard streams piped to this process.
pub fn spawn_quiet(exe: &Path, args: &[&str]) -> Result<Spawned> {
    let pipes = [Pipe::new(true)?, Pipe::new(false)?, Pipe::new(false)?];
    let mut command: Vec<u16> = std::iter::once(quote(&exe.to_string_lossy()))
        .chain(args.iter().map(|a| quote(a)))
        .collect::<Vec<_>>()
        .join(" ")
        .encode_utf16()
        .chain([0])
        .collect();
    // Only the three pipes are inherited, even if another thread starts a
    // process meanwhile with handles of its own marked inheritable.
    let handles = [pipes[0].child, pipes[1].child, pipes[2].child];
    let mut size = 0;
    unsafe {
        let _ = InitializeProcThreadAttributeList(None, 1, None, &mut size);
    }
    let mut buffer = vec![0u8; size];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(buffer.as_mut_ptr().cast());
    let started = unsafe {
        InitializeProcThreadAttributeList(Some(list), 1, None, &mut size)
            .context("could not set up the process's attributes")?;
        let result = (|| {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(handles.as_ptr().cast()),
                size_of_val(&handles),
                None,
                None,
            )
            .context("could not list the handles to pass on")?;
            let mut startup = STARTUPINFOEXW::default();
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES | STARTF_FORCEOFFFEEDBACK;
            startup.StartupInfo.hStdInput = handles[0];
            startup.StartupInfo.hStdOutput = handles[1];
            startup.StartupInfo.hStdError = handles[2];
            startup.lpAttributeList = list;
            let mut info = PROCESS_INFORMATION::default();
            CreateProcessW(
                None,
                Some(PWSTR(command.as_mut_ptr())),
                None,
                None,
                true,
                CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
                None,
                None,
                &startup.StartupInfo,
                &mut info,
            )
            .context("could not start the process")?;
            Ok::<_, anyhow::Error>(info)
        })();
        DeleteProcThreadAttributeList(list);
        result
    };
    let info = match started {
        Ok(info) => info,
        Err(e) => {
            for pipe in pipes {
                drop(pipe.into_parent());
            }
            return Err(e);
        }
    };
    unsafe {
        let _ = CloseHandle(info.hThread);
    }
    let [stdin, stdout, stderr] = pipes.map(Pipe::into_parent);
    Ok(Spawned {
        process: Process(unsafe { OwnedHandle::from_raw_handle(info.hProcess.0) }),
        stdin,
        stdout,
        stderr,
    })
}

/// `arg` quoted for a Windows command line, as the C runtime parses it.
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut quoted = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn arguments_are_quoted_as_windows_reads_them() {
        assert_eq!(quote("--recorder"), "--recorder");
        assert_eq!(
            quote(r"C:\Program Files\Shuttercrab\shuttercrab.exe"),
            r#""C:\Program Files\Shuttercrab\shuttercrab.exe""#
        );
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(r"ends in \"), r#""ends in \\""#);
        assert_eq!(quote(""), r#""""#);
    }

    #[test]
    fn a_quiet_process_talks_through_its_pipes() {
        // `findstr` copies the lines of its input that match to its output.
        let system = std::env::var("SystemRoot").unwrap();
        let exe = Path::new(&system).join(r"System32\findstr.exe");
        let Spawned {
            process,
            mut stdin,
            mut stdout,
            ..
        } = spawn_quiet(&exe, &["x"]).unwrap();
        stdin.write_all(b"one x\r\ntwo\r\nthree x\r\n").unwrap();
        // Closing its input ends it.
        drop(stdin);
        let mut out = String::new();
        stdout.read_to_string(&mut out).unwrap();
        assert_eq!(out, "one x\r\nthree x\r\n");
        assert_eq!(process.wait().unwrap(), 0);
    }
}
