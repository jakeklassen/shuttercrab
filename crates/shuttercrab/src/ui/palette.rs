//! The colours Shuttercrab's windows share. Always dark, like the main
//! window.

use gpui_kit::{Hsla, rgb};

/// Window and bar backgrounds.
pub fn surface() -> Hsla {
    rgb(0x202020).into()
}

/// Buttons and tiles on a surface.
pub fn tile() -> Hsla {
    rgb(0x2B2B2B).into()
}

/// A tile or button under the pointer.
pub fn hover() -> Hsla {
    rgb(0x353535).into()
}

/// Borders around cards, bars and popups.
pub fn border() -> Hsla {
    rgb(0x3A3A3A).into()
}

/// Secondary text and key hints.
pub fn muted() -> Hsla {
    rgb(0x9D9D9D).into()
}

/// The blue of what is selected or about to be captured.
pub fn accent() -> Hsla {
    rgb(0x1F6FEB).into()
}

/// Shuttercrab's coral, the main window's accent.
pub fn coral() -> Hsla {
    rgb(0xE8603C).into()
}

/// The red of a recording in progress, and of Discard.
pub fn recording() -> Hsla {
    rgb(0xE5484D).into()
}

/// The amber of a paused recording.
pub fn paused() -> Hsla {
    rgb(0xF5A524).into()
}
