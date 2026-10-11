//! Linux, as the app names it: the distribution and its version, from
//! `os-release`.

use crate::os::Os;

/// This Linux.
pub fn describe() -> Os {
    let release = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    let (name, version) = parse(&release);
    // Described once and kept for the life of the process.
    let name: &'static str = Box::leak(name.into_boxed_str());
    Os {
        name,
        version_label: name,
        version,
        image_editor: None,
        default_microphone: "The system's default",
        print_screen_setting: None,
        // Wayland lets no app grab keys everywhere; desktop shortcuts run
        // requests instead.
        global_hotkeys: false,
        tray: false,
    }
}

/// The distribution's name and version from `os-release`'s text: `Ubuntu`
/// and `24.04`, or `Linux` and nothing.
fn parse(release: &str) -> (String, String) {
    let field = |key: &str| {
        release.lines().find_map(|line| {
            let value = line.strip_prefix(key)?.strip_prefix('=')?;
            Some(value.trim().trim_matches('"').to_string())
        })
    };
    let name = field("NAME").unwrap_or_else(|| "Linux".into());
    let version = field("VERSION_ID")
        .or_else(|| field("VERSION"))
        .unwrap_or_default();
    (name, version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release_gives_the_name_and_version() {
        let ubuntu = [
            r#"PRETTY_NAME="Ubuntu 24.04.1 LTS""#,
            r#"NAME="Ubuntu""#,
            r#"VERSION_ID="24.04""#,
            r#"VERSION="24.04.1 LTS (Noble Numbat)""#,
            "ID=ubuntu",
        ]
        .join("\n");
        assert_eq!(parse(&ubuntu), ("Ubuntu".into(), "24.04".into()));
        let pop = [r#"NAME="Pop!_OS""#, r#"VERSION="24.04 LTS""#].join("\n");
        assert_eq!(parse(&pop), ("Pop!_OS".into(), "24.04 LTS".into()));
        assert_eq!(parse(""), ("Linux".into(), String::new()));
    }
}
