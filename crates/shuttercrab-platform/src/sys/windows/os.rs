//! Windows, as the app names it: its build, from the kernel rather than the
//! manifest-shimmed API, Paint, and the Settings page for Print Screen.

use crate::os::Os;

/// This Windows.
pub fn describe() -> Os {
    Os {
        name: "Windows",
        version_label: "Windows build",
        version: build().to_string(),
        image_editor: Some("Paint"),
        default_microphone: "Windows' default",
        print_screen_setting: Some("ms-settings:devices-keyboard"),
    }
}

/// The build number, 0 if the kernel does not say.
fn build() -> u32 {
    use windows::{
        Wdk::System::SystemServices::RtlGetVersion,
        Win32::System::SystemInformation::OSVERSIONINFOW,
    };
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    match unsafe { RtlGetVersion(&mut info) }.ok() {
        Ok(()) => info.dwBuildNumber,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_has_a_build_number() {
        let os = describe();
        assert!(os.version.parse::<u32>().unwrap() >= 22000, "{os:?}");
    }
}
