//! The text read from a screenshot ([`shuttercrab_platform::ocr`]), and
//! what Text actions do with it: pick words as a text selection does, in
//! reading order; copy them as lines; find email addresses and phone
//! numbers; and size the black boxes that redact them.

use crate::markup::Redaction;
pub use shuttercrab_platform::ocr::{Line, Rect, Text, Word};

/// A word's place: its line, and its place in the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WordAt {
    pub line: usize,
    pub word: usize,
}

/// Words picked, from the first to the last in reading order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub first: WordAt,
    pub last: WordAt,
}

impl Span {
    /// The words from `a` to `b`, whichever comes first.
    pub fn between(a: WordAt, b: WordAt) -> Span {
        Span {
            first: a.min(b),
            last: a.max(b),
        }
    }

    pub fn contains(self, at: WordAt) -> bool {
        (self.first..=self.last).contains(&at)
    }
}

/// Every word of `text`, in reading order, with its place.
pub fn words(text: &Text) -> impl Iterator<Item = (WordAt, &Word)> {
    text.lines.iter().enumerate().flat_map(|(line, l)| {
        l.words
            .iter()
            .enumerate()
            .map(move |(word, w)| (WordAt { line, word }, w))
    })
}

/// The whole of `text`, if it has any words.
pub fn all(text: &Text) -> Option<Span> {
    let mut words = words(text).map(|(at, _)| at);
    let first = words.next()?;
    Some(Span {
        first,
        last: words.last().unwrap_or(first),
    })
}

/// What a redacted word copies as, as in Snipping Tool.
pub const REDACTED: &str = "[REDACTED]";

/// The words of `span`, as lines: a line's words joined as its language
/// writes them; lines side by side (a line number and its code, say) by a
/// space, and rows by line breaks. Words under one of `hidden` copy as
/// [`REDACTED`], a run of them once.
pub fn copy(text: &Text, span: Span, hidden: &[Redaction]) -> String {
    // Chinese and Japanese leave no spaces between words.
    let joiner = if ["zh", "ja"].iter().any(|l| text.language.starts_with(l)) {
        ""
    } else {
        " "
    };
    let is_hidden = |r: &Rect| {
        let (x, y) = (r.x + r.width / 2., r.y + r.height / 2.);
        hidden
            .iter()
            .any(|h| (h.x..=h.x + h.width).contains(&x) && (h.y..=h.y + h.height).contains(&y))
    };
    let mut rows: Vec<String> = Vec::new();
    let mut current: Option<usize> = None;
    let mut redacting = false;
    for (at, word) in words(text).filter(|(at, _)| span.contains(*at)) {
        let row = rows.last_mut();
        match (current, row) {
            (Some(line), Some(row)) if line == at.line => row.push_str(joiner),
            (Some(line), Some(row)) if same_row(&text.lines[line], &text.lines[at.line]) => {
                row.push(' ');
                redacting = false;
            }
            _ => {
                rows.push(String::new());
                redacting = false;
            }
        }
        current = Some(at.line);
        let Some(row) = rows.last_mut() else {
            continue;
        };
        if !is_hidden(&word.rect) {
            redacting = false;
            row.push_str(&word.text);
        } else if !redacting {
            redacting = true;
            row.push_str(REDACTED);
        } else {
            // One [REDACTED] for the run: take back the joiner.
            row.truncate(row.len() - joiner.len());
        }
    }
    rows.join("\n")
}

/// Whether two lines sit side by side on one row: their middles within
/// half the shorter one's height of each other.
fn same_row(a: &Line, b: &Line) -> bool {
    let (Some(a), Some(b)) = (line_box(a), line_box(b)) else {
        return false;
    };
    let middle = |r: Rect| r.y + r.height / 2.;
    (middle(a) - middle(b)).abs() <= a.height.min(b.height) / 2.
}

/// `text` with its lines in reading order: rows top to bottom, and the
/// lines on a row left to right. OCR gives them grouped its own way (in a
/// code editor, every line number first, then the code).
pub fn reading_order(mut text: Text) -> Text {
    let middle = |line: &Line| line_box(line).map_or(0., |r| r.y + r.height / 2.);
    text.lines.sort_by(|a, b| middle(a).total_cmp(&middle(b)));
    // Rows: each line joins the row before if it sits beside its first.
    let mut rows: Vec<Vec<Line>> = Vec::new();
    for line in text.lines {
        match rows.last_mut() {
            Some(row) if same_row(&row[0], &line) => row.push(line),
            _ => rows.push(vec![line]),
        }
    }
    let left = |line: &Line| line_box(line).map_or(0., |r| r.x);
    for row in &mut rows {
        row.sort_by(|a, b| left(a).total_cmp(&left(b)));
    }
    text.lines = rows.into_iter().flatten().collect();
    text
}

/// The box around a line's words.
pub fn line_box(line: &Line) -> Option<Rect> {
    line.words.iter().map(|w| w.rect).reduce(union)
}

/// The box around both `a` and `b`.
pub fn union(a: Rect, b: Rect) -> Rect {
    let (left, top) = (a.x.min(b.x), a.y.min(b.y));
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    }
}

/// The word under `point` (screenshot pixels), or the nearest one in the
/// line under it: within a line's height of its box, as a text selection
/// starts. `None` away from every line.
pub fn word_near(text: &Text, point: (f32, f32)) -> Option<WordAt> {
    let (x, y) = point;
    let mut best: Option<(f32, WordAt)> = None;
    for (index, line) in text.lines.iter().enumerate() {
        let Some(band) = line_box(line) else {
            continue;
        };
        let reach = band.height;
        let off_y = distance(y, band.y, band.y + band.height);
        let off_x = distance(x, band.x, band.x + band.width);
        if off_y > reach || off_x > reach {
            continue;
        }
        let word = line
            .words
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let d = |w: &Word| distance(x, w.rect.x, w.rect.x + w.rect.width);
                d(a).total_cmp(&d(b))
            })
            .map_or(0, |(i, _)| i);
        let at = WordAt { line: index, word };
        if best.is_none_or(|(d, _)| off_y < d) {
            best = Some((off_y, at));
        }
    }
    best.map(|(_, at)| at)
}

/// The word a drag over `point` reaches, as a text selection does: the
/// nearest line, then the nearest word in it, wherever the pointer is.
pub fn word_toward(text: &Text, point: (f32, f32)) -> Option<WordAt> {
    let (x, y) = point;
    let (line, _) = text
        .lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| Some((i, line_box(line)?)))
        .min_by(|(_, a), (_, b)| {
            let d = |r: &Rect| distance(y, r.y, r.y + r.height);
            d(a).total_cmp(&d(b))
        })?;
    let words = &text.lines[line].words;
    let word = words
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let d = |w: &Word| distance(x, w.rect.x, w.rect.x + w.rect.width);
            d(a).total_cmp(&d(b))
        })
        .map_or(0, |(i, _)| i);
    Some(WordAt { line, word })
}

/// Which way an arrow key moves through the words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Back,
    Forward,
    Up,
    Down,
}

/// The word an arrow key moves to from `from`: the next or the one before
/// in reading order, or the nearest across in the line above or below.
/// At the start or the end it stays.
pub fn step(text: &Text, from: WordAt, step: Step) -> WordAt {
    let order: Vec<WordAt> = words(text).map(|(at, _)| at).collect();
    let Some(index) = order.iter().position(|at| *at == from) else {
        return order.first().copied().unwrap_or(from);
    };
    match step {
        Step::Back => order[index.saturating_sub(1)],
        Step::Forward => order[(index + 1).min(order.len() - 1)],
        Step::Up | Step::Down => {
            let lines = text
                .lines
                .iter()
                .enumerate()
                .filter(|(_, l)| !l.words.is_empty());
            let mut lines: Vec<usize> = lines.map(|(i, _)| i).collect();
            if step == Step::Up {
                lines.reverse();
            }
            let past = |line: &usize| match step {
                Step::Up => *line < from.line,
                _ => *line > from.line,
            };
            let Some(&line) = lines.iter().find(|line| past(line)) else {
                return from;
            };
            let middle = |r: Rect| r.x + r.width / 2.;
            let x = middle(text.lines[from.line].words[from.word].rect);
            let word = text.lines[line]
                .words
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    (middle(a.rect) - x)
                        .abs()
                        .total_cmp(&(middle(b.rect) - x).abs())
                })
                .map_or(0, |(i, _)| i);
            WordAt { line, word }
        }
    }
}

/// `text` with only the words whose middles lie in `area` (a crop), and
/// only the lines left with any.
pub fn within(text: &Text, area: Rect) -> Text {
    let inside = |r: &Rect| {
        let (x, y) = (r.x + r.width / 2., r.y + r.height / 2.);
        (area.x..area.x + area.width).contains(&x) && (area.y..area.y + area.height).contains(&y)
    };
    let lines = text.lines.iter().filter_map(|line| {
        let words: Vec<Word> = line
            .words
            .iter()
            .filter(|w| inside(&w.rect))
            .cloned()
            .collect();
        (!words.is_empty()).then(|| Line {
            text: line.text.clone(),
            words,
        })
    });
    Text {
        language: text.language.clone(),
        angle: text.angle,
        lines: lines.collect(),
    }
}

/// How far `v` lies outside `low..high`: nothing inside.
fn distance(v: f32, low: f32, high: f32) -> f32 {
    (low - v).max(v - high).max(0.)
}

/// What Quick redact looks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Finds {
    pub emails: bool,
    pub phones: bool,
}

/// The email addresses and phone numbers in `text`, as `finds` asks, each
/// as the words it covers.
pub fn sensitive(text: &Text, finds: Finds) -> Vec<Span> {
    let mut found = Vec::new();
    for run in runs(text) {
        let words: Vec<Word> = run
            .iter()
            .map(|at| text.lines[at.line].words[at.word].clone())
            .collect();
        let span = |(first, last): (usize, usize)| Span::between(run[first], run[last]);
        if finds.emails {
            found.extend((0..words.len()).filter_map(|word| email_at(&words, word).map(span)));
        }
        if finds.phones {
            found.extend(phone_runs(&words).into_iter().map(span));
        }
    }
    found.sort_by_key(|span| span.first);
    found
}

/// The words of `text`, in runs of lines that read on from each other: on
/// one row and near, as OCR can read a row as several lines, with a phone
/// number's parts in two. A line far along the row (code after its line
/// number) starts a run of its own. `text` is in reading order.
fn runs(text: &Text) -> Vec<Vec<WordAt>> {
    let mut runs: Vec<Vec<WordAt>> = Vec::new();
    for (line, l) in text.lines.iter().enumerate() {
        let reads_on = line > 0 && {
            let before = &text.lines[line - 1];
            same_row(before, l)
                && line_box(before)
                    .zip(line_box(l))
                    .is_some_and(|(a, b)| b.x - (a.x + a.width) <= 2. * a.height.min(b.height))
        };
        let words = (0..l.words.len()).map(|word| WordAt { line, word });
        match runs.last_mut() {
            Some(run) if reads_on => run.extend(words),
            _ => runs.push(words.collect()),
        }
    }
    runs
}

/// Punctuation that sits around a word in a sentence, not in it.
const OPENERS: &[char] = &['(', '[', '{', '<', '"', '\'', '“', '‘'];
const CLOSERS: &[char] = &[
    ')', ']', '}', '>', '"', '\'', '”', '’', ',', '.', ';', ':', '!', '?',
];

/// Whether `word` is an email address, punctuation around it aside:
/// anything, an @, and a domain with a dot and letters after the last.
/// What comes before the @ may be misread (OCR reads a name underlined by
/// a squiggle as `jß&gsgpgey`), and it is where the address is, not what
/// it says, that a redaction needs; a domain is read well.
fn is_email(word: &str) -> bool {
    email_domain(word).is_some_and(|(local, _)| !local.is_empty())
}

/// The first and last of `words` that the email address with its @ in
/// `word` covers, if it is one. OCR reads some apart: at the @, the word
/// before is its name; at a dot (`jane@example`, `.`, `com` in a code
/// editor), the words after are the rest of its domain, as many as there are.
fn email_at(words: &[Word], word: usize) -> Option<(usize, usize)> {
    let text = |i: usize| words.get(i).map(|w| w.text.as_str());
    // The word, then with each more of its domain: the text and last word.
    let mut joins = vec![(text(word).filter(|w| w.contains('@'))?.to_string(), word)];
    loop {
        let (joined, last) = &joins[joins.len() - 1];
        let longer = match (text(last + 1), text(last + 2)) {
            (Some("."), Some(after)) => (format!("{joined}.{after}"), last + 2),
            (Some(next), _) if next.len() > 1 && next.starts_with('.') => {
                (format!("{joined}{next}"), last + 1)
            }
            _ => break,
        };
        joins.push(longer);
    }
    joins.iter().rev().find_map(|(joined, last)| {
        if is_email(joined) {
            Some((word, *last))
        } else if email_domain(joined).is_some() {
            // Nothing before the @: the word before is its name.
            (word > 0).then(|| (word - 1, *last))
        } else {
            None
        }
    })
}

/// The part of `word` before its last @, and whether what follows is a
/// domain, punctuation around the word aside: `None` without both an @ and
/// a domain.
fn email_domain(word: &str) -> Option<(&str, &str)> {
    let word = word.trim_start_matches(OPENERS).trim_end_matches(CLOSERS);
    let (local, domain) = word.rsplit_once('@')?;
    is_domain(domain).then_some((local, domain))
}

/// Whether `domain` is one: labels of letters, digits and hyphens, at
/// least two, the last all letters.
fn is_domain(domain: &str) -> bool {
    let labels: Vec<&str> = domain.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty() && label.chars().all(|c| c.is_alphanumeric() || c == '-')
        })
        && labels
            .last()
            .is_some_and(|tld| tld.chars().count() >= 2 && tld.chars().all(char::is_alphabetic))
}

/// The runs of words in a line that make phone numbers, by their first and
/// last word: words of digits and `+ ( ) - .`, with 7 to 15 digits in all,
/// as numbers are written around the world. Punctuation after a word ends
/// its run; a date, an IP address or a short plain number is not a phone
/// number.
fn phone_runs(words: &[Word]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut run: Vec<(usize, String)> = Vec::new();
    let mut close = |run: &mut Vec<(usize, String)>| {
        if let (Some((first, _)), Some((last, _))) = (run.first(), run.last())
            && is_phone(run)
        {
            runs.push((*first, *last));
        }
        run.clear();
    };
    for (index, word) in words.iter().enumerate() {
        let trimmed = word.text.trim_end_matches([',', ';', ':', '.']);
        let ends_sentence = trimmed.len() < word.text.len();
        let phone_like = trimmed.chars().any(|c| c.is_ascii_digit())
            && trimmed
                .chars()
                .all(|c| c.is_ascii_digit() || "+()-.".contains(c));
        if !phone_like {
            close(&mut run);
            continue;
        }
        run.push((index, trimmed.to_string()));
        if ends_sentence {
            close(&mut run);
        }
    }
    close(&mut run);
    runs
}

fn is_phone(run: &[(usize, String)]) -> bool {
    let digits: usize = run
        .iter()
        .map(|(_, w)| w.chars().filter(char::is_ascii_digit).count())
        .sum();
    if !(7..=15).contains(&digits) {
        return false;
    }
    let [(_, only)] = run else {
        return true;
    };
    let groups: Vec<&str> = only.split(['-', '.']).collect();
    let sizes: Vec<usize> = groups.iter().map(|g| g.len()).collect();
    let plain = only.chars().all(|c| c.is_ascii_digit());
    // 2026-11-01, 01-11-2026; 192.168.1.20; a 7–9 digit order number.
    let date = matches!(sizes.as_slice(), [4, 2, 2] | [2, 2, 4]) && only.contains('-');
    let address = only.contains('.') && sizes.len() == 4 && sizes.iter().all(|&n| n <= 3);
    !(date || address || (plain && digits < 10))
}

/// The black boxes that hide `span`: one for each row it reaches, from its
/// first word there to its last, the row's full height, and `pad` more
/// each way so no edge of a letter shows. Lines OCR read apart on one row
/// share a box.
pub fn cover(text: &Text, span: Span, pad: f32) -> Vec<Redaction> {
    let mut boxes: Vec<Rect> = Vec::new();
    let mut last_line: Option<&Line> = None;
    for (index, line) in text.lines.iter().enumerate() {
        let Some(band) = line_box(line) else {
            continue;
        };
        let picked = line.words.iter().enumerate().filter(|(word, _)| {
            span.contains(WordAt {
                line: index,
                word: *word,
            })
        });
        let Some(across) = picked.map(|(_, w)| w.rect).reduce(union) else {
            continue;
        };
        let hide = Rect {
            x: across.x - pad,
            y: band.y - pad,
            width: across.width + 2. * pad,
            height: band.height + 2. * pad,
        };
        match boxes.last_mut() {
            Some(last) if last_line.is_some_and(|before| same_row(before, line)) => {
                *last = union(*last, hide);
            }
            _ => boxes.push(hide),
        }
        last_line = Some(line);
    }
    boxes
        .into_iter()
        .map(|r| Redaction {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line of `words`, each 10 pixels a letter and a space apart, on
    /// row `y`; capitals stand a pixel taller.
    fn line(y: f32, words: &[&str]) -> Line {
        let mut x = 0.;
        let words = words
            .iter()
            .map(|text| {
                let width = 10. * text.chars().count() as f32;
                let tall = text.chars().any(char::is_uppercase);
                let rect = Rect {
                    x,
                    y: if tall { y - 1. } else { y },
                    width,
                    height: if tall { 11. } else { 10. },
                };
                x += width + 10.;
                Word {
                    text: text.to_string(),
                    rect,
                }
            })
            .collect();
        Line {
            text: String::new(),
            words,
        }
    }

    fn text(lines: Vec<Line>) -> Text {
        Text {
            language: "en-US".into(),
            angle: None,
            lines,
        }
    }

    fn at(line: usize, word: usize) -> WordAt {
        WordAt { line, word }
    }

    fn sample() -> Text {
        text(vec![
            line(0., &["Hello", "there"]),
            line(20., &["Mail", "jane.doe@example.com,", "or", "call"]),
            line(40., &["+1", "555", "987", "6543", "today."]),
        ])
    }

    #[test]
    fn copying_joins_words_with_spaces_and_lines_with_breaks() {
        let text = sample();
        let span = Span::between(at(1, 2), at(0, 1));
        assert_eq!(
            copy(&text, span, &[]),
            "there\nMail jane.doe@example.com, or"
        );
        assert_eq!(
            copy(&text, all(&text).unwrap(), &[]),
            "Hello there\nMail jane.doe@example.com, or call\n+1 555 987 6543 today."
        );
        // Japanese leaves the spaces out.
        let japanese = Text {
            language: "ja".into(),
            ..text
        };
        assert_eq!(
            copy(&japanese, Span::between(at(0, 0), at(0, 1)), &[]),
            "Hellothere"
        );
        assert_eq!(all(&Text::default()), None);
    }

    #[test]
    fn a_press_near_a_line_finds_its_nearest_word_and_away_from_lines_none() {
        let text = sample();
        // On "there", and in the gap after "Hello" (nearer it).
        assert_eq!(word_near(&text, (75., 5.)), Some(at(0, 1)));
        assert_eq!(word_near(&text, (52., 5.)), Some(at(0, 0)));
        // Just below line 1, nearer it than line 2.
        assert_eq!(word_near(&text, (5., 31.)), Some(at(1, 0)));
        // Far right of every line, and far below.
        assert_eq!(word_near(&text, (900., 5.)), None);
        assert_eq!(word_near(&text, (5., 200.)), None);
        // A drag reaches the nearest word wherever it goes.
        assert_eq!(word_toward(&text, (900., 5.)), Some(at(0, 1)));
        assert_eq!(word_toward(&text, (5., 200.)), Some(at(2, 0)));
    }

    #[test]
    fn redacted_words_copy_as_redacted_once_a_run() {
        let text = sample();
        let finds = Finds {
            emails: true,
            phones: true,
        };
        let hidden: Vec<Redaction> = sensitive(&text, finds)
            .into_iter()
            .flat_map(|span| cover(&text, span, 2.))
            .collect();
        assert_eq!(
            copy(&text, all(&text).unwrap(), &hidden),
            "Hello there\nMail [REDACTED] or call\n[REDACTED] today."
        );
    }

    #[test]
    fn lines_are_put_in_reading_order_and_a_row_copies_as_one() {
        // As OCR gives a code editor: the line numbers first, then the
        // code, the second line before the first.
        let shifted = |mut l: Line, by: f32| {
            for word in &mut l.words {
                word.rect.x += by;
            }
            l
        };
        let text = text(vec![
            line(0., &["1"]),
            line(20., &["2"]),
            shifted(line(20., &["let", "b"]), 40.),
            shifted(line(0., &["let", "a"]), 40.),
        ]);
        let ordered = reading_order(text);
        assert_eq!(
            copy(&ordered, all(&ordered).unwrap(), &[]),
            "1 let a\n2 let b"
        );
    }

    #[test]
    fn arrows_move_word_by_word_and_line_by_line() {
        let text = sample();
        assert_eq!(step(&text, at(0, 1), Step::Forward), at(1, 0));
        assert_eq!(step(&text, at(1, 0), Step::Back), at(0, 1));
        // At either end it stays.
        assert_eq!(step(&text, at(0, 0), Step::Back), at(0, 0));
        assert_eq!(step(&text, at(2, 4), Step::Forward), at(2, 4));
        // Down from "there" (middle at 85): "jane…" spans 50 to 260, but
        // its middle (155) is farther than "Mail"'s (20).
        assert_eq!(step(&text, at(0, 1), Step::Down), at(1, 0));
        // Up from "call" (300 to 340): "there" is the nearest above.
        assert_eq!(step(&text, at(1, 3), Step::Up), at(0, 1));
        assert_eq!(step(&text, at(0, 0), Step::Up), at(0, 0));
    }

    #[test]
    fn a_crop_keeps_the_words_inside_it() {
        let text = sample();
        // The top two lines, left of x 100: "Hello there", and "Mail".
        let area = Rect {
            x: 0.,
            y: -5.,
            width: 100.,
            height: 35.,
        };
        let kept = within(&text, area);
        assert_eq!(copy(&kept, all(&kept).unwrap(), &[]), "Hello there\nMail");
    }

    #[test]
    fn quick_redact_finds_emails_and_phone_numbers() {
        let text = sample();
        let both = Finds {
            emails: true,
            phones: true,
        };
        assert_eq!(
            sensitive(&text, both),
            [
                Span::between(at(1, 1), at(1, 1)),
                Span::between(at(2, 0), at(2, 3)),
            ]
        );
        let emails = Finds {
            emails: true,
            phones: false,
        };
        assert_eq!(sensitive(&text, emails).len(), 1);
    }

    #[test]
    fn quick_redact_reads_on_across_lines_on_one_row() {
        let shift = |mut l: Line, dx: f32| {
            for w in &mut l.words {
                w.rect.x += dx;
            }
            l
        };
        let finds = Finds {
            emails: true,
            phones: true,
        };
        // OCR read the row as two lines, the number's parts in both.
        let row = reading_order(text(vec![
            shift(line(0., &["987", "6543", "today."]), 220.),
            shift(line(0., &["or", "call", "+1", "555"]), 70.),
        ]));
        assert_eq!(sensitive(&row, finds), [Span::between(at(0, 2), at(1, 1))]);
        // One box from +1 to 6543, the gap between the lines too.
        let boxes = cover(&row, Span::between(at(0, 2), at(1, 1)), 2.);
        assert_eq!(boxes.len(), 1);
        assert_eq!((boxes[0].x, boxes[0].width), (148., 154.));
        // A line number far to the left of the code is not part of it.
        let code = reading_order(text(vec![
            line(0., &["548"]),
            shift(line(0., &["555", "1234", "5678"]), 70.),
        ]));
        assert_eq!(sensitive(&code, finds), [Span::between(at(1, 0), at(1, 2))]);
    }

    #[test]
    fn emails_are_told_from_lookalikes() {
        for yes in [
            "jane.doe@example.com",
            "(support@shuttercrab.dev).",
            "a+tag@mail.example.co.uk",
            "<o'brien@example.ie>",
            // A name OCR misread, under an editor's error squiggle.
            "jß&gsgpgey@example.com",
        ] {
            assert!(is_email(yes), "{yes}");
        }
        // Read apart at the @, the word before goes with it.
        let text = text(vec![line(0., &["Mail", "jane", "@example.com", "now"])]);
        let emails = Finds {
            emails: true,
            phones: false,
        };
        assert_eq!(
            sensitive(&text, emails),
            [Span::between(at(0, 1), at(0, 2))]
        );
        // Read apart at a dot, as in a code editor, the words after go too.
        let found = |words: &[&str]| sensitive(&self::text(vec![line(0., words)]), emails);
        assert_eq!(
            found(&["&[\"Mail\",", "\"jane.doe@exampte", ".", "com,\","]),
            [Span::between(at(0, 1), at(0, 3))]
        );
        assert_eq!(
            found(&["to", "jane@example", ".co", ".uk", "now"]),
            [Span::between(at(0, 1), at(0, 3))]
        );
        // But not a sentence after one, nor dots without an @.
        assert!(found(&["at", "me@home.", "Thanks"]).is_empty());
        assert!(found(&["see", "notes", ".", "txt"]).is_empty());
        for no in [
            "@handle",
            "jane@",
            "jane@localhost",
            "a@b.c",
            "x@y.123",
            "@",
        ] {
            assert!(!is_email(no), "{no}");
        }
    }

    #[test]
    fn phone_numbers_are_told_from_dates_amounts_and_addresses() {
        let found = |words: &[&str]| phone_runs(&line(0., words).words);
        assert_eq!(found(&["Call", "(555)", "123-4567,", "now"]), [(1, 2)]);
        assert_eq!(found(&["555.246.8100"]), [(0, 0)]);
        assert_eq!(found(&["+44", "20", "7946", "0958"]), [(0, 3)]);
        assert_eq!(found(&["5551234567"]), [(0, 0)]);
        // Punctuation ends a number: two numbers, not one.
        assert_eq!(found(&["555-123-4567,", "555-987-6543"]), [(0, 0), (1, 1)]);
        for no in [
            &["due", "2026-11-01"][..],
            &["$1,249.99"],
            &["#48213"],
            &["order", "4821377"],
            &["host", "192.168.1.20"],
            &["12:30"],
        ] {
            assert_eq!(found(no), [], "{no:?}");
        }
    }

    #[test]
    fn a_cover_spans_each_line_reached_at_the_lines_full_height() {
        let text = sample();
        // From "there" to "Mail": one box on each line.
        let boxes = cover(&text, Span::between(at(0, 1), at(1, 0)), 2.);
        assert_eq!(boxes.len(), 2);
        // "there" alone sits lower than "Hello", but the box takes the
        // line's height: from Hello's top, a pixel higher.
        assert_eq!(
            boxes[0],
            Redaction {
                x: 58.,
                y: -3.,
                width: 54.,
                height: 15.
            }
        );
        assert_eq!((boxes[1].x, boxes[1].width), (-2., 44.), "Mail alone");
    }
}
