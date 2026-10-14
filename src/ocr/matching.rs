//! Finds e-mail addresses and phone numbers in recognized lines. A line is matched as one
//! text joined from its words, so a number the engine split into several words
//! ("+90 532 123 45 67") is still found, and every match maps back to the words it spans.
use super::{OcrLine, OcrWord, WordBox};

/// Put between two words too far apart to belong together, such as neighbouring table
/// columns; no match spans it.
const BREAK: char = '\n';
/// A phone number has 7 to 15 digits (E.164 allows 15).
const MIN_PHONE_DIGITS: usize = 7;
const MAX_PHONE_DIGITS: usize = 15;
/// Without a `+`, an area code in parentheses or a `00` prefix, a number written in groups
/// has at most this many digits; a longer run is two numbers side by side.
const MAX_NATIONAL_DIGITS: usize = 13;

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
        found.extend(phones_in_run(chars, index, end));
        index = end;
    }
    found
}

/// The numbers in the run `start..end` of digits and phone separators. A run may hold
/// several numbers, a number next to a date or an order number, or parts that no phone
/// number is written with, so the groups of digits are split into the numbers that cover
/// the most digits. A number is never cut out of the middle of a date.
fn phones_in_run(chars: &[char], start: usize, end: usize) -> Vec<(usize, usize)> {
    let groups = digit_groups(chars, start, end);
    let blocked = date_groups(chars, &groups);
    // `best[i]`: the digits the numbers made of groups `i..` cover at most, and the last
    // group and first character of the number starting at group `i` that achieves it.
    let mut best: Vec<(usize, Option<(usize, usize)>)> = vec![(0, None); groups.len() + 1];
    for i in (0..groups.len()).rev() {
        let mut top: (usize, Option<(usize, usize)>) = (best[i + 1].0, None);
        let candidates = run_candidates(chars, start, &groups, &blocked, i);
        for (last, first, digits) in candidates.into_iter().rev() {
            let covered = digits + best[last + 1].0;
            if covered > top.0 || (covered == top.0 && top.1.is_none()) {
                top = (covered, Some((last, first)));
            }
        }
        best[i] = top;
    }
    let mut found = Vec::new();
    let mut i = 0;
    while i < groups.len() {
        match best[i].1 {
            Some((last, first)) => {
                found.push((first, groups[last].1));
                i = last + 1;
            }
            None => i += 1,
        }
    }
    found
}

/// The character range of each group of digits in `start..end`.
fn digit_groups(chars: &[char], start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut groups = Vec::new();
    let mut index = start;
    while index < end {
        if !chars[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let from = index;
        while index < end && chars[index].is_ascii_digit() {
            index += 1;
        }
        groups.push((from, index));
    }
    groups
}

/// Which groups belong to a date written with one separator between day, month and year:
/// 15.01.2024, 2024-01-15.
fn date_groups(chars: &[char], groups: &[(usize, usize)]) -> Vec<bool> {
    let mut blocked = vec![false; groups.len()];
    for (index, trio) in groups.windows(3).enumerate() {
        let first = &chars[trio[0].1..trio[1].0];
        let second = &chars[trio[1].1..trio[2].0];
        let [a, b, c] = [trio[0], trio[1], trio[2]].map(|(from, to)| &chars[from..to]);
        if first == second && matches!(first, ['.'] | ['-']) && is_date(a, b, c) {
            blocked[index..index + 3].fill(true);
        }
    }
    blocked
}

/// The numbers that start at group `from`: for each group they could end at, `(that group,
/// the first character, the digits)`, a leading `+` or `(` included.
fn run_candidates(
    chars: &[char],
    run_start: usize,
    groups: &[(usize, usize)],
    blocked: &[bool],
    from: usize,
) -> Vec<(usize, usize, usize)> {
    let plain = groups[from].0;
    let prefixed = (plain > run_start && matches!(chars[plain - 1], '+' | '(')).then(|| plain - 1);
    let mut found = Vec::new();
    let mut digits = 0;
    for last in from..groups.len() {
        let joined = last == from || valid_separator(&chars[groups[last - 1].1..groups[last].0]);
        digits += groups[last].1 - groups[last].0;
        if blocked[last] || !joined || digits > MAX_PHONE_DIGITS {
            break;
        }
        let end = groups[last].1;
        if let Some(first) = prefixed
            .into_iter()
            .chain(Some(plain))
            .find(|&first| is_phone(chars, first, end))
        {
            found.push((last, first, digits));
        }
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
/// word or an amount of money, not part of a decimal, a time or a percentage, and not the
/// digits of a `+` number whose sign is not part of the match.
fn standalone(chars: &[char], start: usize, end: usize) -> bool {
    let before = start.checked_sub(1).map(|index| chars[index]);
    let after = chars.get(end).copied();
    let is_digit = |index: usize| chars.get(index).is_some_and(char::is_ascii_digit);
    let glued_before = before.is_some_and(|ch| {
        ch.is_alphanumeric() || matches!(ch, '_' | '+' | '$' | '€' | '£' | '¥' | '₺')
    }) || (matches!(before, Some(',' | '.' | ':'))
        && start.checked_sub(2).is_some_and(is_digit));
    let glued_after = after.is_some_and(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '%' | ':'))
        || (matches!(after, Some(',' | '.')) && is_digit(end + 1));
    !glued_before && !glued_after
}

/// A candidate split into its groups of digits and the separators between them.
pub(super) struct Phone<'a> {
    /// A `+` prefix or an area code in parentheses says the writer meant a phone number.
    formatted: bool,
    /// Starts with the `00` of an international call.
    international: bool,
    groups: Vec<&'a [char]>,
    separators: Vec<&'a [char]>,
}

impl<'a> Phone<'a> {
    /// `span` starts with a digit, `+` or `(` that is followed by a digit. `None` when its
    /// parentheses or separators are not the ones a phone number is written with.
    pub(super) fn parse(span: &'a [char]) -> Option<Self> {
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
            international: groups[0].starts_with(&['0', '0']),
            groups,
            separators,
        })
    }

    /// Whether the digits read as a phone number rather than a date, a version, an address,
    /// an amount or a list of figures.
    fn plausible(&self) -> bool {
        let digits: usize = self.groups.iter().map(|group| group.len()).sum();
        let most = if self.formatted || self.international {
            MAX_PHONE_DIGITS
        } else {
            MAX_NATIONAL_DIGITS
        };
        if !(MIN_PHONE_DIGITS..=most).contains(&digits) {
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
            && !self.is_time_range()
            && !self.is_year_range()
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

    /// Day, month and year in either order and with any separator: 15.01.2024, 15 01 2024,
    /// 2024-01-15.
    fn is_date(&self) -> bool {
        matches!(self.groups.as_slice(), [a, b, c] if is_date(a, b, c))
    }

    /// Opening hours: 09.00-18.00.
    fn is_time_range(&self) -> bool {
        let [a, b, c, d] = self.groups.as_slice() else {
            return false;
        };
        let separators: &[&[char]] = &self.separators;
        separators == [&['.'][..], &['-'][..], &['.'][..]]
            && [a, c]
                .iter()
                .all(|hour| hour.len() <= 2 && number(hour) <= 24)
            && [b, d]
                .iter()
                .all(|minute| minute.len() == 2 && number(minute) <= 59)
    }

    /// Two years: 2020-2024.
    fn is_year_range(&self) -> bool {
        let [from, to] = self.groups.as_slice() else {
            return false;
        };
        [from, to]
            .iter()
            .all(|year| year.len() == 4 && (1900..=2099).contains(&number(year)))
    }
}

/// Whether three groups of digits read as day, month and year in either order.
fn is_date(a: &[char], b: &[char], c: &[char]) -> bool {
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
