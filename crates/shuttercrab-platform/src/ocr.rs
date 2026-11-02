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

/// How tall, in pixels, [`read_closer`] draws a line's text. An 18-pixel
/// line in a table OCR read `name17@` in as `namel`, `7@` read whole at
/// three times that, and still apart at twice (2026-11-02).
const CLOSER_TALL: f32 = 54.;

/// Read again, alone and larger, each line of `text` that `pick` picks,
/// and keep what that read finds inside the line in place of its words.
/// OCR reads a word apart where a narrow letter leaves a gap (the `1` in
/// `name17@example.com` as `namel`, `7@example.com`), and whole drawn
/// larger. A line read as nothing the second time stays as it was. `bgra`
/// is the `width` × `height` image `text` was read from.
pub fn read_closer(
    bgra: &[u8],
    width: u32,
    height: u32,
    text: &mut Text,
    pick: impl Fn(&Line) -> bool,
) -> anyhow::Result<()> {
    let language = text.language.clone();
    closer(bgra, width, height, text, pick, |strip, w, h, enlarge| {
        read(strip, w, h, Some(&language), enlarge)
    })
}

/// [`read_closer`], reading each strip with `read(bgra, width, height,
/// enlarge)`.
fn closer(
    bgra: &[u8],
    width: u32,
    height: u32,
    text: &mut Text,
    pick: impl Fn(&Line) -> bool,
    mut read: impl FnMut(&[u8], u32, u32, u32) -> anyhow::Result<Text>,
) -> anyhow::Result<()> {
    for line in text.lines.iter_mut().filter(|line| pick(line)) {
        let Some(bounds) = bounds(&line.words) else {
            continue;
        };
        let enlarge = (CLOSER_TALL / bounds.height).ceil().clamp(2., 4.) as u32;
        // Room around the line, as OCR reads a word badly at an image's edge.
        let room = bounds.height / 2.;
        let left = (bounds.x - room).max(0.) as u32;
        let top = (bounds.y - room).max(0.) as u32;
        let right = ((bounds.x + bounds.width + room).ceil() as u32).min(width);
        let bottom = ((bounds.y + bounds.height + room).ceil() as u32).min(height);
        if right <= left || bottom <= top {
            continue;
        }
        let strip = crop(bgra, width, (left, top, right - left, bottom - top));
        let inside = |word: &Word| {
            let (x, y) = (
                word.rect.x + word.rect.width / 2.,
                word.rect.y + word.rect.height / 2.,
            );
            (bounds.x..=bounds.x + bounds.width).contains(&x)
                && (bounds.y..=bounds.y + bounds.height).contains(&y)
        };
        let words: Vec<Word> = read(&strip, right - left, bottom - top, enlarge)?
            .lines
            .into_iter()
            .flat_map(|line| line.words)
            .map(|word| Word {
                rect: Rect {
                    x: word.rect.x + left as f32,
                    y: word.rect.y + top as f32,
                    ..word.rect
                },
                ..word
            })
            .filter(inside)
            .collect();
        if !words.is_empty() {
            line.text = words
                .iter()
                .map(|word| word.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            line.words = words;
        }
    }
    Ok(())
}

/// The box around all of `words`.
fn bounds(words: &[Word]) -> Option<Rect> {
    words.iter().map(|word| word.rect).reduce(|a, b| {
        let (x, y) = (a.x.min(b.x), a.y.min(b.y));
        Rect {
            x,
            y,
            width: (a.x + a.width).max(b.x + b.width) - x,
            height: (a.y + a.height).max(b.y + b.height) - y,
        }
    })
}

/// The pixels of `(x, y, width, height)` in a `stride_width`-wide BGRA image.
fn crop(bgra: &[u8], stride_width: u32, (x, y, width, height): (u32, u32, u32, u32)) -> Vec<u8> {
    let (row, stride) = (width as usize * 4, stride_width as usize * 4);
    let mut pixels = Vec::with_capacity(row * height as usize);
    for line in y as usize..(y + height) as usize {
        let at = line * stride + x as usize * 4;
        pixels.extend_from_slice(&bgra[at..at + row]);
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, x: f32, y: f32, width: f32) -> Word {
        Word {
            text: text.into(),
            rect: Rect {
                x,
                y,
                width,
                height: 18.,
            },
        }
    }

    fn line(words: Vec<Word>) -> Line {
        let text = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>();
        Line {
            text: text.join(" "),
            words,
        }
    }

    #[test]
    fn a_closer_read_takes_the_place_of_a_lines_words() {
        let mut text = Text {
            lines: vec![
                line(vec![word("Total", 100., 20., 50.)]),
                line(vec![
                    word("namel", 100., 100., 50.),
                    word("7@example.com", 153., 100., 120.),
                ]),
            ],
            ..Text::default()
        };
        let image = vec![0; 400 * 200 * 4];
        let mut asked = Vec::new();
        closer(
            &image,
            400,
            200,
            &mut text,
            |l| l.text.contains('@'),
            |_, w, h, by| {
                asked.push((w, h, by));
                // In the strip's pixels, which start 9 left of and above the
                // line: the address whole, and the line above's edge.
                Ok(Text {
                    lines: vec![
                        line(vec![word("Total", 9., -71., 50.)]),
                        line(vec![word("name17@example.com", 9., 9., 173.)]),
                    ],
                    ..Text::default()
                })
            },
        )
        .unwrap();
        // Read alone with half a line of room around, three times as large.
        assert_eq!(asked, [(191, 36, 3)]);
        assert_eq!(
            text.lines[1],
            line(vec![word("name17@example.com", 100., 100., 173.)])
        );
        assert_eq!(text.lines[0].words.len(), 1, "a line not picked stays");

        // A line read as nothing the second time stays as it was.
        let before = text.clone();
        closer(
            &image,
            400,
            200,
            &mut text,
            |_| true,
            |_, _, _, _| Ok(Text::default()),
        )
        .unwrap();
        assert_eq!(text, before);
    }

    #[test]
    fn a_crop_takes_the_rows_and_columns_asked() {
        // 3 × 2 pixels, each byte its index.
        let image: Vec<u8> = (0..24).collect();
        assert_eq!(
            crop(&image, 3, (1, 0, 2, 2)),
            [4, 5, 6, 7, 8, 9, 10, 11, 16, 17, 18, 19, 20, 21, 22, 23]
        );
    }

    #[test]
    fn small_scales_are_read_twice_as_large() {
        assert_eq!(enlargement(1.), 2);
        assert_eq!(enlargement(1.25), 2);
        assert_eq!(enlargement(1.5), 1);
        assert_eq!(enlargement(2.), 1);
    }
}
