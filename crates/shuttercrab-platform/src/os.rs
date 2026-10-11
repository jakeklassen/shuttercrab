//! The OS Shuttercrab runs on: what it is called, and what it offers that
//! other OSes may not, so the app names things as the OS does and offers
//! only what is there.

/// The OS, as [`os`] describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Os {
    /// Its name, as its users know it: `Windows`.
    pub name: &'static str,
    /// What its version is called, and the version, for the log and
    /// Settings: `Windows build` and `26300`.
    pub version_label: &'static str,
    pub version: String,
    /// The image editor everyone has, which Edit in opens: `Paint`.
    pub image_editor: Option<&'static str>,
    /// What the microphone the OS picks is called: `Windows' default`.
    pub default_microphone: &'static str,
    /// The page of the OS's settings that lets it give up Print Screen, if
    /// it can keep the key for its own screenshots.
    pub print_screen_setting: Option<&'static str>,
    /// Whether Shuttercrab can register hotkeys that work in every app.
    /// Where it cannot (Wayland), a desktop shortcut runs a [`crate::Request`]
    /// on the command line instead.
    pub global_hotkeys: bool,
    /// Whether Shuttercrab has a tray icon to stay in when its window
    /// closes. Without one, closing the window quits.
    pub tray: bool,
}

/// This OS, described once.
pub fn os() -> &'static Os {
    static OS: std::sync::OnceLock<Os> = std::sync::OnceLock::new();
    OS.get_or_init(crate::sys::imp::os::describe)
}
