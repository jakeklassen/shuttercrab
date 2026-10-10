//! Cursors GPUI does not offer, shown over part of a window: the open and
//! closed hands and the four-way move arrow.

pub use crate::sys::imp::cursor::{Cursor, show};

/// A cursor over a part of a window: client pixels, x, y, width, height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorOver {
    pub cursor: Cursor,
    pub area: (i32, i32, i32, i32),
}
