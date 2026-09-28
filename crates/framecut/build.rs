//! Embeds the application icon (framecut.rc) in the executable.

fn main() {
    println!("cargo:rerun-if-changed=framecut.rc");
    println!("cargo:rerun-if-changed=assets/framecut.ico");
    embed_resource::compile("framecut.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("could not compile framecut.rc");
}
