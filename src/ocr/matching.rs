//! Finds e-mail addresses and phone numbers in recognized lines. A line is matched as one
//! text joined from its words, so a number the engine split into several words
//! ("+90 532 123 45 67") is still found, and every match maps back to the words it spans.
use super::{OcrLine, OcrWord, WordBox};
use std::ops::RangeInclusive;

/// Put between two words too far apart to belong together, such as neighbouring table
/// columns; no match spans it.
const BREAK: char = '\n';
/// A phone number has 7 to 15 digits (E.164 allows 15).
const PHONE_DIGITS: RangeInclusive<usize> = 7..=15;

/// The union of the word boxes of every e-mail address and phone number in `lines`, line by
/// line in reading order.
pub(super) fn sensitive_boxes(lines: &[OcrLine]) -> Vec<WordBox> {
    lines
        .iter()
        .flat_map(|line| line_boxes(&line.words))
        .collect()
}

fn line_boxes(words: &[OcrWord]) -> Vec<WordBox> {
    let line = LineText::new(words);
    let emails = find_emails(&line.chars);
    // A phone number is never read out of an address.
    let mut without_emails = line.chars.clone();
    for &(start, end) in &emails {
        without_emails[start..end].fill(BREAK);
    }
    let mut found = emails;
    found.extend(find_phones(&without_emails));
    found.sort_unstable();
    found
        .into_iter()
        .filter_map(|(start, end)| line.bounds(start, end))
        .collect()
}

/// The words of a line as one run of characters, remembering which word each came from.
struct LineText<'a> {
    words: &'a [OcrWord],
    chars: Vec<char>,
    /// The word each character belongs to; `None` for the separators added between words.
    owner: Vec<Option<usize>>,
}

impl<'a> LineText<'a> {
    fn new(words: &'a [OcrWord]) -> Self {
        let mut chars = Vec::new();
        let mut owner = Vec::new();
        let mut before: Option<&OcrWord> = None;
        for (index, word) in words.iter().enumerate() {
            if let Some(separator) = before.and_then(|before| separator(before, word)) {
                chars.push(separator);
                owner.push(None);
            }
            for ch in word.text.chars() {
                chars.push(ch);
                owner.push(Some(index));
            }
            before = Some(word);
        }
        Self {
            words,
            chars,
            owner,
        }
    }

    /// The union of the boxes of the words that `start..end` of the text touches; `None`
    /// when an end of the range is not on a word.
    fn bounds(&self, start: usize, end: usize) -> Option<WordBox> {
        let first = self.owner[start]?;
        let last = self.owner[end - 1]?;
        self.words[first..=last]
            .iter()
            .map(|word| word.bounds)
            .reduce(WordBox::union)
    }
}

/// What sits between two neighbouring words: nothing next to an `@` the engine split off, a
/// break where the words are farther apart than a line is tall, a space otherwise.
fn separator(before: &OcrWord, after: &OcrWord) -> Option<char> {
    if before.text.ends_with('@') || after.text.starts_with('@') {
        return None;
    }
    let gap = after.bounds.left - before.bounds.right;
    let height = before.bounds.height().max(after.bounds.height());
    Some(if gap > height { BREAK } else { ' ' })
}

// ---- e-mail addresses ---------------------------------------------------------------

fn is_local(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '.' | '_' | '%' | '+' | '-')
}

fn is_domain(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '.' | '-')
}

/// The character ranges of the e-mail addresses in `chars`.
fn find_emails(chars: &[char]) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = chars[from..].iter().position(|&ch| ch == '@') {
        let at = from + offset;
        from = at + 1;
        if let Some((start, end)) = email_around(chars, at) {
            found.push((start, end));
            from = end;
        }
    }
    found
}

/// The address the `@` at `at` belongs to: a local part before it and a domain with a
/// top-level domain after it.
fn email_around(chars: &[char], at: usize) -> Option<(usize, usize)> {
    let mut start = at;
    while start > 0 && is_local(chars[start - 1]) {
        start -= 1;
    }
    // A local part does not start with a dot or a hyphen; those belong to the text before.
    while start < at && matches!(chars[start], '.' | '-') {
        start += 1;
    }
    if start == at {
        return None;
    }
    let mut end = at + 1;
    while end < chars.len() && is_domain(chars[end]) {
        end += 1;
    }
    // A domain does not end with a dot or a hyphen either: a sentence may.
    while end > at + 1 && matches!(chars[end - 1], '.' | '-') {
        end -= 1;
    }
    valid_domain(&chars[at + 1..end]).then_some((start, end))
}

/// Dot-separated labels that do not start or end with a hyphen, the last one a top-level
/// domain of letters only.
fn valid_domain(domain: &[char]) -> bool {
    let mut labels = domain.rsplit(|&ch| ch == '.');
    let top = labels.next().unwrap_or_default();
    let mut hosts = labels.peekable();
    top.len() >= 2
        && top.iter().all(|ch| ch.is_alphabetic())
        && hosts.peek().is_some()
        && hosts.all(|label| !label.is_empty() && label[0] != '-' && label[label.len() - 1] != '-')
}

// ---- phone numbers ------------------------------------------------------------------

/// The character ranges of the phone numbers in `chars`.
fn find_phones(chars: &[char]) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if !starts_phone(chars, index) {
            index += 1;
            continue;
        }
        let end = phone_span_end(chars, index);
        if is_phone(chars, index, end) {
            found.push((index, end));
        }
        index = end;
    }
    found
}

fn starts_phone(chars: &[char], index: usize) -> bool {
    match chars[index] {
        '0'..='9' => true,
        '+' | '(' => chars.get(index + 1).is_some_and(char::is_ascii_digit),
        _ => false,
    }
}

/// The end of the run of digits, `+ - . ( )` and spaces that starts at `start`; it stops
/// after the last digit or closing parenthesis, never on a separator.
fn phone_span_end(chars: &[char], start: usize) -> usize {
    let mut end = start;
    let mut open = 0;
    for (offset, &ch) in chars[start..].iter().enumerate() {
        match ch {
            '0'..='9' => end = start + offset + 1,
            ')' if open > 0 => {
                open -= 1;
                end = start + offset + 1;
            }
            '(' => open += 1,
            '+' | ' ' | '-' | '.' => {}
            _ => break,
        }
    }
    end
}

fn is_phone(chars: &[char], start: usize, end: usize) -> bool {
    Phone::parse(&chars[start..end]).is_some_and(|phone| phone.plausible())
        && standalone(chars, start, end)
}

/// Whether the text around `start..end` leaves it a number of its own: not the tail of a
/// word or an amount of money, not part of a decimal, a time or a percentage.
fn standalone(chars: &[char], start: usize, end: usize) -> bool {
    let before = start.checked_sub(1).map(|index| chars[index]);
    let after = chars.get(end).copied();
    let is_digit = |index: usize| chars.get(index).is_some_and(char::is_ascii_digit);
    let glued_before = before
        .is_some_and(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '$' | '€' | '£' | '¥' | '₺'))
        || (matches!(before, Some(',' | ':')) && start.checked_sub(2).is_some_and(is_digit));
    let glued_after = after.is_some_and(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '%' | ':'))
        || (after == Some(',') && is_digit(end + 1));
    !glued_before && !glued_after
}

/// A candidate split into its groups of digits and the separators between them.
struct Phone<'a> {
    /// A `+` prefix or an area code in parentheses says the writer meant a phone number.
    formatted: bool,
    groups: Vec<&'a [char]>,
    separators: Vec<&'a [char]>,
}

impl<'a> Phone<'a> {
    /// `span` starts with a digit, `+` or `(` that is followed by a digit. `None` when its
    /// parentheses or separators are not the ones a phone number is written with.
    fn parse(span: &'a [char]) -> Option<Self> {
        let plus = span[0] == '+';
        let open = span.iter().position(|&ch| ch == '(');
        let close = span.iter().position(|&ch| ch == ')');
        let area_code = match (open, close) {
            (None, None) => false,
            // "(212) 555-0199": digits alone in the parentheses, more digits after them.
            (Some(open), Some(close))
                if open < close
                    && span[open + 1..close].iter().all(char::is_ascii_digit)
                    && span[span.len() - 1].is_ascii_digit() =>
            {
                true
            }
            _ => return None,
        };
        let lead = usize::from(!span[0].is_ascii_digit());
        let mut groups = Vec::new();
        let mut separators = Vec::new();
        for part in span[lead..].chunk_by(|a, b| a.is_ascii_digit() == b.is_ascii_digit()) {
            if part[0].is_ascii_digit() {
                groups.push(part);
            } else if valid_separator(part) {
                separators.push(part);
            } else {
                return None;
            }
        }
        Some(Self {
            formatted: plus || area_code,
            groups,
            separators,
        })
    }

    /// Whether the digits read as a phone number rather than a date, a version, an address,
    /// an amount or a list of figures.
    fn plausible(&self) -> bool {
        let digits: usize = self.groups.iter().map(|group| group.len()).sum();
        if !PHONE_DIGITS.contains(&digits) {
            return false;
        }
        if self.formatted {
            return true;
        }
        let dotted = self.only('.');
        let dotted_phone = !dotted
            || (self.groups.len() >= 3
                && self
                    .groups
                    .iter()
                    .all(|group| (2..=4).contains(&group.len()))
                && !self.is_ipv4());
        // 12.345.678 or 1 234 567: an amount with thousands separators.
        let thousands = (dotted || self.only(' '))
            && self.groups[0].len() <= 3
            && self.groups[1..].iter().all(|group| group.len() == 3);
        // Only the first group may be a single digit: "1 2 3 4 5 6 7" is a list, not a number.
        self.groups[1..].iter().all(|group| group.len() >= 2)
            && dotted_phone
            && !thousands
            && !self.is_date()
    }

    /// Every separator is the single character `wanted`.
    fn only(&self, wanted: char) -> bool {
        !self.separators.is_empty()
            && self
                .separators
                .iter()
                .all(|&separator| separator == [wanted].as_slice())
    }

    fn is_ipv4(&self) -> bool {
        self.groups.len() == 4
            && self
                .groups
                .iter()
                .all(|group| group.len() <= 3 && number(group) <= 255)
    }

    /// Day, month and year in either order and with any separator: 15.01.2024, 01/15/2024,
    /// 2024-01-15.
    fn is_date(&self) -> bool {
        let [a, b, c] = self.groups.as_slice() else {
            return false;
        };
        let (x, y, z) = (number(a), number(b), number(c));
        match (a.len(), b.len(), c.len()) {
            (1..=2, 1..=2, 4) => {
                (1900..=2099).contains(&z)
                    && (1..=31).contains(&x)
                    && (1..=31).contains(&y)
                    && x.min(y) <= 12
            }
            (4, 1..=2, 1..=2) => {
                (1900..=2099).contains(&x) && (1..=12).contains(&y) && (1..=31).contains(&z)
            }
            _ => false,
        }
    }
}

/// One or two of space, hyphen, dot and parenthesis, not the same twice in a row.
fn valid_separator(separator: &[char]) -> bool {
    separator
        .iter()
        .all(|&ch| matches!(ch, ' ' | '-' | '.' | '(' | ')'))
        && match separator {
            [_] => true,
            [first, second] => first != second,
            _ => false,
        }
}

/// The value of a group of ASCII digits.
fn number(group: &[char]) -> u64 {
    group.iter().fold(0, |total, &digit| {
        total * 10 + u64::from(digit as u8 - b'0')
    })
}
