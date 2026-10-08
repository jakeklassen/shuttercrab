//! Reading the text in a screenshot with Windows' own OCR engine
//! (`Windows.Media.Ocr`): on the device, in the languages whose OCR packs
//! are installed (Settings > Time & language > Language, a language's
//! "Optical character recognition" feature).
//!
//! The engine blocks while it reads, so call [`read`] off the UI thread, on
//! one in the multithreaded apartment.

use anyhow::{Context, Result, bail};
use windows::{
    Globalization::Language,
    Graphics::Imaging::{
        BitmapAlphaMode, BitmapBufferAccessMode, BitmapPixelFormat, SoftwareBitmap,
    },
    Media::Ocr::{OcrEngine, OcrLine},
    Win32::System::WinRT::IMemoryBufferByteAccess,
    core::{HSTRING, Interface},
};

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

/// The languages OCR can read here: those with an OCR pack installed.
pub fn languages() -> Result<Vec<OcrLanguage>> {
    let available =
        OcrEngine::AvailableRecognizerLanguages().context("could not list the OCR languages")?;
    available
        .into_iter()
        .map(|language| {
            Ok(OcrLanguage {
                tag: language.LanguageTag()?.to_string(),
                name: language.DisplayName()?.to_string(),
            })
        })
        .collect()
}

/// The longest side an image may have: larger ones must be read in parts.
pub fn max_side() -> Result<u32> {
    OcrEngine::MaxImageDimension().context("could not ask OCR for its largest image")
}

/// Read the text in a `width` × `height` image of BGRA pixels, as GPUI
/// draws them. `language` is a BCP-47 tag; `None` reads in the first of
/// the user's languages that OCR has, or failing that any it has.
pub fn read(bgra: &[u8], width: u32, height: u32, language: Option<&str>) -> Result<Text> {
    if bgra.len() != (width as usize * height as usize * 4) || width == 0 || height == 0 {
        bail!("image buffer does not match {width}x{height}");
    }
    let most = max_side()?;
    if width > most || height > most {
        bail!("OCR reads images up to {most} pixels a side; this is {width}x{height}");
    }
    let engine = engine(language)?;
    let bitmap = bitmap(bgra, width, height)?;
    let result = engine
        .RecognizeAsync(&bitmap)
        .context("could not start OCR")?
        .join()
        .context("OCR failed")?;
    let lines = result
        .Lines()?
        .into_iter()
        .map(|line| line_of(&line))
        .collect::<Result<_>>()?;
    Ok(Text {
        language: engine.RecognizerLanguage()?.LanguageTag()?.to_string(),
        // Null when the engine could not tell.
        angle: result.TextAngle().and_then(|angle| angle.Value()).ok(),
        lines,
    })
}

/// An engine for `language`, or for the user's languages.
fn engine(language: Option<&str>) -> Result<OcrEngine> {
    if let Some(tag) = language {
        let language = Language::CreateLanguage(&HSTRING::from(tag))
            .with_context(|| format!("{tag:?} is not a language tag"))?;
        if !OcrEngine::IsLanguageSupported(&language)? {
            bail!("no OCR pack for {tag} is installed");
        }
        return OcrEngine::TryCreateFromLanguage(&language)
            .with_context(|| format!("could not start OCR in {tag}"));
    }
    // Null, an error here, when none of the user's languages has a pack.
    if let Ok(engine) = OcrEngine::TryCreateFromUserProfileLanguages() {
        return Ok(engine);
    }
    let Some(first) = OcrEngine::AvailableRecognizerLanguages()?
        .into_iter()
        .next()
    else {
        bail!("no OCR language is installed");
    };
    OcrEngine::TryCreateFromLanguage(&first).context("could not start OCR")
}

/// A `SoftwareBitmap` holding a copy of the pixels, row by row into its
/// own stride.
fn bitmap(bgra: &[u8], width: u32, height: u32) -> Result<SoftwareBitmap> {
    let bitmap = SoftwareBitmap::CreateWithAlpha(
        BitmapPixelFormat::Bgra8,
        width as i32,
        height as i32,
        BitmapAlphaMode::Ignore,
    )
    .context("could not make a bitmap for OCR")?;
    {
        let buffer = bitmap.LockBuffer(BitmapBufferAccessMode::Write)?;
        let plane = buffer.GetPlaneDescription(0)?;
        let reference = buffer.CreateReference()?;
        let access: IMemoryBufferByteAccess = reference.cast()?;
        let (mut data, mut capacity) = (std::ptr::null_mut(), 0);
        // SAFETY: the buffer is locked for writing until `reference` and
        // `buffer` drop at the end of this block.
        unsafe { access.GetBuffer(&mut data, &mut capacity)? };
        let (row, stride) = (width as usize * 4, plane.Stride as usize);
        let start = plane.StartIndex as usize;
        if start + stride * (height as usize - 1) + row > capacity as usize {
            bail!("the OCR bitmap's buffer is smaller than its image");
        }
        for y in 0..height as usize {
            // SAFETY: checked just above to lie within the buffer.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bgra.as_ptr().add(y * row),
                    data.add(start + y * stride),
                    row,
                );
            }
        }
        reference.Close()?;
        buffer.Close()?;
    }
    Ok(bitmap)
}

fn line_of(line: &OcrLine) -> Result<Line> {
    let words = line
        .Words()?
        .into_iter()
        .map(|word| {
            let rect = word.BoundingRect()?;
            Ok(Word {
                text: word.Text()?.to_string(),
                rect: Rect {
                    x: rect.X,
                    y: rect.Y,
                    width: rect.Width,
                    height: rect.Height,
                },
            })
        })
        .collect::<Result<_>>()?;
    Ok(Line {
        text: line.Text()?.to_string(),
        words,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mismatched_buffers_are_refused_before_ocr_starts() {
        assert!(read(&[0; 12], 2, 2, None).is_err());
        assert!(read(&[], 0, 0, None).is_err());
    }
}
