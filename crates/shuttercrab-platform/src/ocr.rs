//! Reading the text in a screenshot with the OS's own text recognition, on
//! the device. The engine blocks while it reads, so call [`read`] off the
//! UI thread.

/// The text found in an image.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Text {
    /// The language it was read as, a BCP-47 tag such as `en-US`.
    pub language: String,
    /// How far the text is turned from level, degrees clockwise, if the
    /// engine could tell. Boxes are in the image's own axes either way.
    pub angle: Option<f64>,
    pub lines: Vec<Line>,
}

/// One line of text, top to bottom as the engine reads them.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// The line's words joined as the language writes them: with spaces,
    /// or none in Chinese and Japanese.
    pub text: String,
    pub words: Vec<Word>,
}

/// One word and where it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    /// Its box, image pixels.
    pub rect: Rect,
}

/// A box, image pixels from the top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// An OCR language installed on this PC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OcrLanguage {
    /// Its BCP-47 tag, such as `en-US`.
    pub tag: String,
    /// Its name as Windows shows it, such as "English (United States)".
    pub name: String,
}

pub use crate::sys::imp::ocr::{languages, max_side, read};

/// How much larger to read a screenshot taken at display `scale`: text at
/// 100% and 125% is small enough that OCR misreads it (9 as g, $ as 5,
/// words run together), and reads right drawn twice as large and smooth.
/// From 150% it reads right as it is.
pub fn enlargement(scale: f32) -> u32 {
    if scale < 1.5 { 2 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_scales_are_read_twice_as_large() {
        assert_eq!(enlargement(1.), 2);
        assert_eq!(enlargement(1.25), 2);
        assert_eq!(enlargement(1.5), 1);
        assert_eq!(enlargement(2.), 1);
    }
}
