//! The backend for an OS Shuttercrab does not support yet: everything is
//! there, so the app builds and starts. What would change the screen or
//! the system does nothing, what is asked for is absent, and what must
//! succeed fails with an error saying it is not supported here.

/// The error for `what`, which this OS cannot do yet.
fn unsupported(what: &str) -> anyhow::Error {
    anyhow::anyhow!("{what} is not supported on this OS yet")
}

pub mod platform {
    use super::unsupported;
    use crate::{Hotkey, HotkeyConflict, MenuItem, PlatformEvent, Tray};
    use anyhow::Result;
    use futures::channel::mpsc;
    use shuttercrab_types::WindowId;
    use std::{future::Future, path::PathBuf, sync::Arc};

    /// The platform thread, with nothing to do: no tray, no hotkeys.
    pub struct Platform {
        /// Keeps the event stream open.
        _events: mpsc::UnboundedSender<PlatformEvent>,
    }

    impl Platform {
        pub fn start(
            _hotkeys: &[(u32, Hotkey)],
            _tray: Option<Tray>,
        ) -> Result<(
            Self,
            mpsc::UnboundedReceiver<PlatformEvent>,
            Vec<HotkeyConflict>,
        )> {
            let (events, stream) = mpsc::unbounded();
            Ok((Self { _events: events }, stream, Vec::new()))
        }

        pub fn copy_image(
            &self,
            _png: Arc<Vec<u8>>,
            _rgba: Arc<Vec<u8>>,
            _width: u32,
            _height: u32,
        ) -> impl Future<Output = Result<()>> + use<> {
            std::future::ready(Err(unsupported("The clipboard")))
        }

        pub fn copy_files(&self, _paths: Vec<PathBuf>) -> impl Future<Output = Result<()>> + use<> {
            std::future::ready(Err(unsupported("The clipboard")))
        }

        pub fn set_hotkeys(
            &self,
            _hotkeys: Vec<(u32, Hotkey)>,
        ) -> impl Future<Output = Vec<HotkeyConflict>> + use<> {
            std::future::ready(Vec::new())
        }

        pub fn set_tray_menu(&self, _menu: Vec<MenuItem>) {}

        pub fn watch_window(&self, _window: Option<WindowId>) {}

        pub fn notify(&self, title: impl Into<String>, message: impl Into<String>) {
            log::info!("{}: {}", title.into(), message.into());
        }
    }

    pub fn signal_running_instance() -> bool {
        false
    }
}

pub mod console {
    pub fn attach_to_parent_terminal() {}

    pub fn detach_from_terminal() {}
}

pub mod instance {
    /// Held while this is the only Shuttercrab running. Nothing checks here.
    pub struct SingleInstance;

    pub fn single_instance(_name: &str) -> Option<SingleInstance> {
        Some(SingleInstance)
    }
}

pub mod watch {
    use crate::frame::Rect;
    use shuttercrab_types::WindowId;

    pub fn visible_bounds(_window: WindowId) -> Option<Rect> {
        None
    }
}

pub mod window {
    use super::unsupported;
    use anyhow::Result;
    use raw_window_handle::HasWindowHandle;
    use shuttercrab_types::{MonitorId, WindowId};

    pub fn outer_bounds(_window: WindowId) -> Result<(i32, i32, u32, u32)> {
        Err(unsupported("Window placement"))
    }

    pub fn place(_window: WindowId, _bounds: (i32, i32, u32, u32)) -> Result<()> {
        Err(unsupported("Window placement"))
    }

    pub fn sound_process(_window: WindowId) -> Option<u32> {
        None
    }

    pub fn monitor_of(_window: WindowId) -> MonitorId {
        MonitorId::from_raw(0)
    }

    pub fn cover(_window: WindowId, _x: i32, _y: i32, _width: u32, _height: u32) -> Result<()> {
        Err(unsupported("Window placement"))
    }

    pub fn client_bounds(_window: WindowId) -> Result<(i32, i32, u32, u32)> {
        Err(unsupported("Window placement"))
    }

    pub fn bring_to_front(_window: WindowId) {}

    pub fn foreground_window() -> Option<WindowId> {
        None
    }

    pub fn hide(_window: WindowId) {}

    pub fn exclude_from_capture(_window: WindowId) -> Result<()> {
        Err(unsupported("Keeping a window out of captures"))
    }

    pub fn include_in_capture(_window: WindowId) {}

    pub fn is_on_screen(_window: WindowId) -> bool {
        false
    }

    pub fn round_corners(_window: WindowId) {}

    pub fn never_activate(_window: WindowId) {}

    pub fn work_area(_monitor: MonitorId) -> Option<(i32, i32, u32, u32)> {
        None
    }

    pub fn fit_client_area(
        _window: WindowId,
        _size: (u32, u32),
        _share: f32,
        _least: (u32, u32),
    ) -> Result<()> {
        Err(unsupported("Window placement"))
    }

    pub fn show_normal(_window: WindowId) {}

    pub fn of(_window: &impl HasWindowHandle) -> Option<WindowId> {
        None
    }
}

pub mod targets {
    use crate::targets::WindowTarget;

    pub fn visible_windows() -> Vec<WindowTarget> {
        Vec::new()
    }
}

pub mod cursor {
    use super::unsupported;
    use crate::cursor::CursorOver;
    use anyhow::Result;
    use shuttercrab_types::WindowId;

    /// A cursor the OS cannot show yet.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Cursor;

    impl Cursor {
        pub fn from_rgba(_rgba: &[u8], _size: u32, _hotspot: (u32, u32)) -> Result<Cursor> {
            Err(unsupported("Custom cursors"))
        }

        pub fn move_all() -> Result<Cursor> {
            Err(unsupported("Custom cursors"))
        }
    }

    pub fn show(_window: WindowId, _over: Option<CursorOver>) {}
}

pub mod drag {
    use super::unsupported;
    use crate::drag::DragImage;
    use anyhow::Result;
    use shuttercrab_types::WindowId;
    use std::path::Path;

    pub fn drag_file(_path: &Path, _image: Option<DragImage>) -> Result<bool> {
        Err(unsupported("Dragging a file out"))
    }

    pub fn refuse_drops(_window: WindowId) -> Result<()> {
        Ok(())
    }
}

pub mod frame {
    use super::unsupported;
    use crate::frame::{FrameStyle, Rect};
    use anyhow::Result;
    use shuttercrab_types::WindowId;

    /// A recording border the OS cannot show yet.
    pub struct Frame;

    impl Frame {
        pub fn show(
            _area: Rect,
            _bounds: Rect,
            _style: FrameStyle,
            _color: [u8; 3],
        ) -> Result<(Self, usize)> {
            Err(unsupported("The recording border"))
        }

        pub fn windows(&self) -> Vec<WindowId> {
            Vec::new()
        }

        pub fn recolor(&mut self, _color: [u8; 3]) -> Result<()> {
            Ok(())
        }

        pub fn place(&mut self, _area: Rect, _bounds: Rect) -> Result<()> {
            Ok(())
        }
    }
}

pub mod hotkey {
    pub fn print_screen_taken() -> bool {
        false
    }
}

pub mod memory {
    use crate::memory::Usage;

    pub(crate) fn usage() -> Usage {
        Usage::default()
    }
}

pub mod ocr {
    use super::unsupported;
    use crate::ocr::{OcrLanguage, Text};
    use anyhow::Result;

    pub fn languages() -> Result<Vec<OcrLanguage>> {
        Ok(Vec::new())
    }

    pub fn max_side() -> Result<u32> {
        Err(unsupported("Reading text"))
    }

    pub fn read(
        _bgra: &[u8],
        _width: u32,
        _height: u32,
        _language: Option<&str>,
        _enlarge: u32,
    ) -> Result<Text> {
        Err(unsupported("Reading text"))
    }
}

pub mod open {
    use super::unsupported;
    use anyhow::Result;
    use std::path::Path;

    pub fn edit_in_paint(_path: &Path) -> Result<()> {
        Err(unsupported("Opening an image editor"))
    }

    pub fn open_with(_path: &Path) {}
}

pub mod os {
    use crate::os::Os;

    pub fn describe() -> Os {
        Os {
            name: std::env::consts::OS,
            version_label: std::env::consts::OS,
            version: String::new(),
            image_editor: None,
            default_microphone: "The default microphone",
            print_screen_setting: None,
        }
    }
}

pub mod process {
    use super::unsupported;
    use crate::process::Spawned;
    use anyhow::Result;
    use std::path::Path;

    /// A process the OS cannot start yet.
    pub struct Process;

    impl Process {
        pub fn wait(&self) -> Result<u32> {
            Err(unsupported("Helper processes"))
        }
    }

    pub fn spawn_quiet(_exe: &Path, _args: &[&str]) -> Result<Spawned> {
        Err(unsupported("Helper processes"))
    }
}

pub mod startup {
    use super::unsupported;
    use anyhow::Result;

    pub fn launch_at_startup() -> bool {
        false
    }

    pub fn set_launch_at_startup(_enabled: bool) -> Result<()> {
        Err(unsupported("Launching at sign-in"))
    }

    pub fn add_background_flag() -> Result<bool> {
        Ok(false)
    }
}
