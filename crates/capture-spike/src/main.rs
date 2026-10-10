//! capture-spike: Shuttercrab's Milestone 0 executable, a Windows tool for
//! measuring capture and colour. Run with no arguments for usage.

#[cfg(windows)]
mod tool;

#[cfg(windows)]
fn main() {
    tool::main();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("capture-spike measures Windows capture; it does nothing on this OS");
    std::process::exit(1);
}
