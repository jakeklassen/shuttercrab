//! Reading the text in a screenshot with Windows' own OCR engine
//! (`Windows.Media.Ocr`): on the device, in the languages whose OCR packs
//! are installed (Settings > Time & language > Language, a language's
//! "Optical character recognition" feature).
//!
//! The engine blocks while it reads, so call [`read`] off the UI thread, on
//! one in the multithreaded apartment.

use crate::ocr::{Line, OcrLanguage, Rect, Text, Word};
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
/// draws them, `enlarge` times larger (smoothly; less if that would pass
/// [`max_side`]). Boxes are in the image's own pixels either way.
/// `language` is a BCP-47 tag; `None` reads in the first of the user's
/// languages that OCR has, or failing that any it has.
pub fn read(
    bgra: &[u8],
    width: u32,
    height: u32,
    language: Option<&str>,
    enlarge: u32,
) -> Result<Text> {
    if bgra.len() != (width as usize * height as usize * 4) || width == 0 || height == 0 {
        bail!("image buffer does not match {width}x{height}");
    }
    let most = max_side()?;
    if width > most || height > most {
        bail!("OCR reads images up to {most} pixels a side; this is {width}x{height}");
    }
    let by = enlarge.clamp(1, most / width.max(height));
    let engine = engine(language)?;
    let bitmap = bitmap(width * by, height * by, |y, row| {
        if by == 1 {
            let at = y * width as usize * 4;
            row.copy_from_slice(&bgra[at..at + row.len()]);
        } else {
            enlarged_row(bgra, width, height, by, y, row);
        }
    })?;
    let result = engine
        .RecognizeAsync(&bitmap)
        .context("could not start OCR")?
        .join()
        .context("OCR failed")?;
    let lines = result
        .Lines()?
        .into_iter()
        .map(|line| line_of(&line, by as f32))
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

/// A `width` × `height` `SoftwareBitmap` whose rows `fill` writes, top to
/// bottom, straight into its own buffer.
fn bitmap(
    width: u32,
    height: u32,
    mut fill: impl FnMut(usize, &mut [u8]),
) -> Result<SoftwareBitmap> {
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
        // `buffer` close at the end of this block.
        unsafe { access.GetBuffer(&mut data, &mut capacity)? };
        let (row, stride) = (width as usize * 4, plane.Stride as usize);
        let start = plane.StartIndex as usize;
        if stride < row || start + stride * (height as usize - 1) + row > capacity as usize {
            bail!("the OCR bitmap's buffer is smaller than its image");
        }
        for y in 0..height as usize {
            // SAFETY: checked just above to lie within the buffer, which
            // nothing else touches while it is locked.
            let target =
                unsafe { std::slice::from_raw_parts_mut(data.add(start + y * stride), row) };
            fill(y, target);
        }
        reference.Close()?;
        buffer.Close()?;
    }
    Ok(bitmap)
}

/// Row `y` of the `width` × `height` image `bgra`, `by` times larger:
/// each pixel blended from the four nearest (bilinear), so text stays
/// smooth. Blocky, pixel-repeated text reads worse than the original.
fn enlarged_row(bgra: &[u8], width: u32, height: u32, by: u32, y: usize, row: &mut [u8]) {
    let source = |n: usize, most: u32| {
        let at = ((n as f32 + 0.5) / by as f32 - 0.5).clamp(0., (most - 1) as f32);
        let low = at.floor() as usize;
        (low, (low + 1).min(most as usize - 1), at - low as f32)
    };
    let (y0, y1, fy) = source(y, height);
    let line = width as usize * 4;
    let (top, bottom) = (&bgra[y0 * line..][..line], &bgra[y1 * line..][..line]);
    for (x, out) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x0, x1, fx) = source(x, width);
        for c in 0..4 {
            let blend = |row: &[u8]| {
                f32::from(row[x0 * 4 + c]) * (1. - fx) + f32::from(row[x1 * 4 + c]) * fx
            };
            out[c] = (blend(top) * (1. - fy) + blend(bottom) * fy).round() as u8;
        }
    }
}

/// A line as OCR found it, its boxes brought back from an image `by`
/// times larger.
fn line_of(line: &OcrLine, by: f32) -> Result<Line> {
    let words = line
        .Words()?
        .into_iter()
        .map(|word| {
            let rect = word.BoundingRect()?;
            Ok(Word {
                text: word.Text()?.to_string(),
                rect: Rect {
                    x: rect.X / by,
                    y: rect.Y / by,
                    width: rect.Width / by,
                    height: rect.Height / by,
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
        assert!(read(&[0; 12], 2, 2, None, 1).is_err());
        assert!(read(&[], 0, 0, None, 1).is_err());
    }

    #[test]
    fn enlarging_blends_between_pixels_and_keeps_the_edges() {
        // One row, black then white, twice as wide.
        let bgra = [0, 0, 0, 255, 255, 255, 255, 255];
        let mut row = [0; 16];
        enlarged_row(&bgra, 2, 1, 2, 0, &mut row);
        let firsts: Vec<u8> = row.as_chunks::<4>().0.iter().map(|px| px[0]).collect();
        // The ends stay as they were; between, a quarter and three quarters.
        assert_eq!(firsts, [0, 64, 191, 255]);
        assert!(row.as_chunks::<4>().0.iter().all(|px| px[3] == 255));
    }
}
