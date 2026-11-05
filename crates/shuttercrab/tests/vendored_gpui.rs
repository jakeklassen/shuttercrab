//! Shuttercrab builds against its own copy of GPUI's Windows layer
//! (`vendor/gpui-pre-windows`, see its `SHUTTERCRAB.md`). When GPUI moves to a
//! newer `gpui-pre-windows` than the copy, Cargo only warns that the patch is
//! unused and quietly builds the unpatched crate from crates.io instead, so
//! every fix in the copy would be lost with the build still passing. These
//! tests fail in that case: update the copy as `SHUTTERCRAB.md` describes.

use std::path::Path;

fn workspace_file(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The lockfile's entry for `gpui-pre-windows`: the lines after its name,
/// up to the next package.
fn locked_gpui_pre_windows(lock: &str) -> Vec<&str> {
    let mut lines = lock.lines();
    lines
        .by_ref()
        .find(|line| *line == r#"name = "gpui-pre-windows""#)
        .expect("Cargo.lock has no gpui-pre-windows");
    lines.take_while(|line| !line.is_empty()).collect()
}

#[test]
fn no_patch_goes_unused() {
    let lock = workspace_file("Cargo.lock");
    assert!(
        !lock.contains("[[patch.unused]]"),
        "a [patch.crates-io] entry is unused (see [[patch.unused]] in Cargo.lock); \
         update vendor/gpui-pre-windows as its SHUTTERCRAB.md describes"
    );
}

#[test]
fn gpui_pre_windows_is_the_vendored_copy() {
    let lock = workspace_file("Cargo.lock");
    let entry = locked_gpui_pre_windows(&lock);
    assert!(
        !entry.iter().any(|line| line.starts_with("source = ")),
        "gpui-pre-windows comes from a registry, not vendor/gpui-pre-windows; \
         update the copy as its SHUTTERCRAB.md describes"
    );

    let manifest = workspace_file("vendor/gpui-pre-windows/Cargo.toml");
    let vendored = manifest
        .lines()
        .find_map(|line| line.strip_prefix("version = "))
        .expect("vendor/gpui-pre-windows/Cargo.toml has no version");
    let locked = entry
        .iter()
        .find_map(|line| line.strip_prefix("version = "))
        .expect("Cargo.lock's gpui-pre-windows has no version");
    assert_eq!(
        locked, vendored,
        "Cargo.lock and the vendored copy disagree on the version"
    );
}
