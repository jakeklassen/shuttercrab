//! Cursors GPUI does not offer, shown over part of a window: the open and
//! closed hands and the four-way move arrow.

pub use crate::sys::imp::cursor::{Cursor, show};
use shuttercrab_types::PhysicalRect;

/// A cursor over a part of a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorOver {
    pub cursor: Cursor,
    /// The part, in the window's client pixels.
    pub area: PhysicalRect,
}
