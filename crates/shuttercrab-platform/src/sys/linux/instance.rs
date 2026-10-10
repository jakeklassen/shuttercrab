//! One Shuttercrab per user on Linux: a Unix socket in the user's runtime
//! folder. The first copy listens on it, and its platform thread turns what
//! a second copy sends into [`PlatformEvent::AnotherInstance`]; the second
//! sends its [`Request`] and exits.
//!
//! [`PlatformEvent::AnotherInstance`]: crate::PlatformEvent::AnotherInstance

use crate::Request;
use std::{
    io::Write,
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    sync::Mutex,
};

/// The name the app gives [`single_instance`].
const NAME: &str = "Shuttercrab";

/// The socket, once bound, until the platform thread takes it.
static LISTENER: Mutex<Option<UnixListener>> = Mutex::new(None);

/// This copy is the only one running while it is held; dropping it removes
/// the socket.
pub struct SingleInstance {
    path: Option<PathBuf>,
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Where the socket for `name` is: the user's runtime folder, or the shared
/// temporary folder with the user's name.
fn socket(name: &str) -> PathBuf {
    let name = name.to_lowercase();
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join(format!("{name}.sock")),
        None => {
            let user = std::env::var("USER").unwrap_or_default();
            std::env::temp_dir().join(format!("{name}-{user}.sock"))
        }
    }
}

/// Hold the socket for `name`, or `None` if another copy holds it. A socket
/// nobody answers on was left by a copy that crashed, and is replaced.
pub fn single_instance(name: &str) -> Option<SingleInstance> {
    let path = socket(name);
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => Some(listener),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if UnixStream::connect(&path).is_ok() {
                return None;
            }
            let _ = std::fs::remove_file(&path);
            UnixListener::bind(&path).ok()
        }
        // Nowhere to put it: run without the check.
        Err(e) => {
            log::warn!("no single-instance socket at {}: {e}", path.display());
            None
        }
    };
    let held = listener.is_some();
    if let Ok(mut slot) = LISTENER.lock() {
        *slot = listener;
    }
    Some(SingleInstance {
        path: held.then_some(path),
    })
}

/// The socket [`single_instance`] bound, for the platform thread to listen
/// on.
pub(crate) fn take_listener() -> Option<UnixListener> {
    LISTENER.lock().ok()?.take()
}

/// Tell the running Shuttercrab that another copy was started, and what it
/// asks for. Returns whether one was found.
pub fn signal_running_instance(request: Request) -> bool {
    UnixStream::connect(socket(NAME))
        .and_then(|mut stream| writeln!(stream, "{}", request.name()))
        .is_ok()
}
