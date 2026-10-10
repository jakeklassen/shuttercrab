//! Dragging a file out of Shuttercrab into other applications (PRD §7.6),
//! with a picture of it under the pointer.

pub use crate::sys::imp::drag::{drag_file, refuse_drops};

/// The picture under the pointer while dragging: straight-alpha RGBA8, top
/// row first, drawn as given (any softness or transparency baked in),
/// centred on the pointer.
pub struct DragImage<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
}
