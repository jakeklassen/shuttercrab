//! A URL scheme of the user's own (`HKCU\Software\Classes\SCHEME`), which
//! Windows opens by running a command with the link.

use anyhow::{Context, Result};
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows_registry::CURRENT_USER;

/// Where Windows looks for `scheme`.
fn key(scheme: &str) -> String {
    format!(r"Software\Classes\{scheme}")
}

/// Open `scheme:` links by running this program with `flag` and the link,
/// named `description` where Windows asks which app to use.
pub fn register(scheme: &str, flag: &str, description: &str) -> Result<()> {
    let exe = std::env::current_exe().context("no path for this executable")?;
    let key = key(scheme);
    // The key's default value has no name.
    let set = |path: &str, name: &str, value: &str| {
        CURRENT_USER
            .create(path)
            .and_then(|k| k.set_string(name, value))
            .with_context(|| format!("could not write {path}"))
    };
    set(&key, "", &format!("URL:{description}"))?;
    set(&key, "URL Protocol", "")?;
    set(
        &format!(r"{key}\DefaultIcon"),
        "",
        &format!("\"{}\",0", exe.display()),
    )?;
    set(
        &format!(r"{key}\shell\open\command"),
        "",
        &format!("\"{}\" {flag} \"%1\"", exe.display()),
    )
}

/// Stop opening `scheme:` links.
pub fn unregister(scheme: &str) -> Result<()> {
    match CURRENT_USER.remove_tree(key(scheme)) {
        Err(e) if e.code() != ERROR_FILE_NOT_FOUND.to_hresult() => {
            Err(e).context("could not remove the link scheme")
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Removes the test's scheme even if the test fails.
    struct TestScheme(&'static str);

    impl Drop for TestScheme {
        fn drop(&mut self) {
            let _ = CURRENT_USER.remove_tree(key(self.0));
        }
    }

    #[test]
    fn a_scheme_is_registered_and_removed() {
        let scheme = TestScheme("shuttercrab-test-scheme");
        register(scheme.0, "--watch", "Shuttercrab test").unwrap();
        let read = |path: &str| CURRENT_USER.open(path).unwrap().get_string("").unwrap();
        assert_eq!(read(&key(scheme.0)), "URL:Shuttercrab test");
        let protocol = CURRENT_USER.open(key(scheme.0)).unwrap();
        assert_eq!(protocol.get_string("URL Protocol").unwrap(), "");
        let command = read(&format!(r"{}\shell\open\command", key(scheme.0)));
        assert!(command.ends_with(r#"" --watch "%1""#), "{command}");
        unregister(scheme.0).unwrap();
        assert!(CURRENT_USER.open(key(scheme.0)).is_err());
        // Removing it again is not an error.
        unregister(scheme.0).unwrap();
    }
}
