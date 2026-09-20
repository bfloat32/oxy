//! The duration vocabulary: the unit words, the numbers people say rather
//! than type, decimal counts, and the one-word compactions (`1h30m`, `90s`).
//! Straight ports of the script's `unit_seconds`, `word_number`,
//! `hundredths`, `compact_seconds` and `starts_duration`, kept apart from the
//! loop that walks them so `parse` reads like the shell it replaced.

/// `unit_seconds` — one unit word to its length.
pub(super) fn unit_seconds(w: &str) -> Option<i64> {
    Some(match w.to_lowercase().as_str() {
        "d" | "day" | "days" => 86400,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        _ => return None,
    })
}

/// `word_number` — the numbers people say rather than type. "Seventy" was
/// never taught, and a word that is not here belongs to the message, not to
/// the count.
pub(super) fn word_number(w: &str) -> Option<i64> {
    Some(match w.to_lowercase().as_str() {
        "one" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        "eleven" => 11,
        "twelve" => 12,
        "thirteen" => 13,
        "fourteen" => 14,
        "fifteen" => 15,
        "sixteen" => 16,
        "seventeen" => 17,
        "eighteen" => 18,
        "nineteen" => 19,
        "twenty" => 20,
        "thirty" => 30,
        "forty" | "fourty" => 40,
        "fifty" => 50,
        "sixty" => 60,
        "ninety" => 90,
        _ => return None,
    })
}

/// `hundredths` — a count that may carry a decimal point, in either of the
/// two ways a decimal point gets written: "1.5h" and "1,5h" are the same hour
/// and a half. Returns hundredths, so the caller can multiply without floats.
pub(super) fn hundredths(n: &str) -> Option<i64> {
    let n = n.replace(',', ".");
    if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) {
        return n.parse::<i64>().ok().map(|v| v.wrapping_mul(100));
    }
    let (whole, frac) = n.split_once('.')?;
    // `[0-9]*\.[0-9]{1,2}` — the whole part may be empty (".5"), the fraction
    // may not, and two digits is the most precision a reminder can use.
    if !whole.bytes().all(|b| b.is_ascii_digit())
        || !(1..=2).contains(&frac.len())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let w = if whole.is_empty() {
        0
    } else {
        whole.parse::<i64>().ok()?
    };
    let f = if frac.len() == 1 {
        frac.parse::<i64>().ok()? * 10
    } else {
        frac.parse::<i64>().ok()?
    };
    Some(w.wrapping_mul(100).wrapping_add(f))
}

/// `^[0-9]+([.,][0-9]{1,2})?$` — a count, whole or with a decimal point. The
/// shape a bare word must have before `hundredths` is asked to read it.
pub(super) fn is_count(w: &str) -> bool {
    let digits = w.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return false;
    }
    let rest = &w[digits..];
    if rest.is_empty() {
        return true;
    }
    let Some(frac) = rest.strip_prefix(['.', ',']) else {
        return false;
    };
    (1..=2).contains(&frac.len()) && frac.bytes().all(|b| b.is_ascii_digit())
}

/// The unit spellings `compact_seconds` knows, longest first — `minutes`
/// beats `min` beats `m`, the choice POSIX leftmost-longest gave the
/// script's alternation.
const UNITS: &[&str] = &[
    "minutes", "seconds", "minute", "second", "hours", "mins", "secs", "hour", "days", "min",
    "sec", "hrs", "day", "hr", "d", "h", "m", "s",
];

/// The longest unit the string opens with, if it opens with one.
fn unit_prefix(s: &str) -> Option<usize> {
    UNITS.iter().find(|u| s.starts_with(**u)).map(|u| u.len())
}

/// The ends `[0-9]+([.,][0-9]{1,2})?` could rest at, longest first. The
/// decimal tail is tried two digits, then one, then not at all — a candidate
/// only counts if a unit can still stand after it.
fn num_ends(s: &str, digits: usize) -> Vec<usize> {
    let mut ends = Vec::new();
    if matches!(s.as_bytes().get(digits), Some(b'.') | Some(b',')) {
        let frac = s
            .bytes()
            .skip(digits + 1)
            .take_while(|b| b.is_ascii_digit())
            .count();
        if frac >= 2 {
            ends.push(digits + 3);
        }
        if frac >= 1 {
            ends.push(digits + 2);
        }
    }
    ends.push(digits);
    ends
}

/// One `count unit` chunk off the front of `s`:
/// `^([0-9]+([.,][0-9]{1,2})?)(unit)(.*)$`.
fn take_chunk(s: &str) -> Option<(&str, &str, &str)> {
    let digits = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    for end in num_ends(s, digits) {
        if let Some(ulen) = unit_prefix(&s[end..]) {
            return Some((&s[..end], &s[end..end + ulen], &s[end + ulen..]));
        }
    }
    None
}

/// `^([0-9]{1,2})h([0-5][0-9])$` — "2h30" is how a duration gets written
/// when the trailing `m` is not worth the keystroke. The `h` clock spellings
/// (`19h30`) never reach here — the clock read claims them first.
fn h_minute_shape(rest: &str) -> Option<String> {
    let (h, m) = rest.split_once('h')?;
    if !(1..=2).contains(&h.len())
        || m.len() != 2
        || !h.bytes().all(|b| b.is_ascii_digit())
        || !m.bytes().all(|b| b.is_ascii_digit())
        || m.parse::<i64>().ok()? > 59
    {
        return None;
    }
    Some(format!("{rest}m"))
}

/// `compact_seconds` — one word holding its own units: `1h30m`, `90s`, `5m`.
/// The word must be entirely chunks; a trailing letter with no count, or a
/// count with no unit, ends it.
pub(super) fn compact_seconds(word: &str) -> Option<i64> {
    let mut rest = word.to_lowercase();
    if !rest.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    if let Some(hm) = h_minute_shape(&rest) {
        rest = hm;
    }
    let mut total = 0i64;
    while let Some((num, unit, tail)) = take_chunk(&rest) {
        let step = unit_seconds(unit)?;
        let hun = hundredths(num)?;
        total = total.wrapping_add(hun.wrapping_mul(step) / 100);
        rest = tail.to_string();
    }
    if !rest.is_empty() || total <= 0 {
        return None;
    }
    Some(total)
}

/// `starts_duration` — whether a word opens another chunk of the time. This
/// is what decides that the "and" in "30 minutes and 45 seconds" is part of
/// the time, while the "and" in "5m and call mum" is part of the message.
pub(super) fn starts_duration(w: &str) -> bool {
    if w.is_empty() {
        return false;
    }
    let w = w.to_lowercase();
    matches!(w.as_str(), "a" | "an" | "half" | "quarter")
        || is_count(&w)
        || word_number(&w).is_some()
        || compact_seconds(&w).is_some()
}

/// `([a-z]+)$` of a compact word, as seconds — the unit a trailing fraction
/// borrows when it brings none of its own ("1h30m and a half" borrows the
/// `m`). Sixty when the word ended in a digit.
pub(super) fn trailing_unit(w: &str) -> i64 {
    let letters = w
        .bytes()
        .rev()
        .take_while(|b| b.is_ascii_lowercase())
        .count();
    unit_seconds(&w[w.len() - letters..]).unwrap_or(60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units() {
        assert_eq!(unit_seconds("minutes"), Some(60));
        assert_eq!(unit_seconds("H"), Some(3600));
        assert_eq!(unit_seconds("day"), Some(86400));
        assert_eq!(unit_seconds("sec"), Some(1));
        assert_eq!(unit_seconds("fortnight"), None);
        assert_eq!(unit_seconds(""), None);
    }

    #[test]
    fn numbers_as_words() {
        assert_eq!(word_number("twenty"), Some(20));
        assert_eq!(word_number("fourty"), Some(40)); // the misspelling counts too
        assert_eq!(word_number("ninety"), Some(90));
        // never taught — "seventy minutes" is a message, not a count
        assert_eq!(word_number("seventy"), None);
        assert_eq!(word_number("hundred"), None);
    }

    #[test]
    fn counts_in_hundredths() {
        assert_eq!(hundredths("45"), Some(4500));
        assert_eq!(hundredths("1.5"), Some(150));
        assert_eq!(hundredths("1,5"), Some(150)); // the other decimal point
        assert_eq!(hundredths(".5"), Some(50));
        assert_eq!(hundredths("1."), None);
        assert_eq!(hundredths("1.555"), None);
        assert_eq!(hundredths("abc"), None);
    }

    #[test]
    fn count_shapes() {
        assert!(is_count("5"));
        assert!(is_count("2.5"));
        assert!(is_count("2,5"));
        assert!(!is_count("5m"));
        assert!(!is_count(".5"));
        assert!(!is_count("1.555"));
    }

    #[test]
    fn compact_words() {
        assert_eq!(compact_seconds("5m"), Some(300));
        assert_eq!(compact_seconds("1h30m"), Some(5400));
        assert_eq!(compact_seconds("1h30m45s"), Some(5445));
        assert_eq!(compact_seconds("90s"), Some(90));
        assert_eq!(compact_seconds("3d"), Some(259200));
        assert_eq!(compact_seconds("1.5h"), Some(5400));
        assert_eq!(compact_seconds("1hr30m"), Some(5400));
        // the trailing-m shorthand: "2h30" is two and a half hours here
        assert_eq!(compact_seconds("2h30"), Some(9000));
        // longest-first: "5minutes" is five minutes, not 5 "inutes"
        assert_eq!(compact_seconds("5minutes"), Some(300));
        assert_eq!(compact_seconds("45seconds"), Some(45));
        // leftovers or a zero total refuse the word
        assert_eq!(compact_seconds("5mx"), None);
        assert_eq!(compact_seconds("0s"), None);
        assert_eq!(compact_seconds("tea"), None);
        assert_eq!(compact_seconds("5"), None); // no unit, no compact
    }

    #[test]
    fn duration_openers() {
        assert!(starts_duration("45"));
        assert!(starts_duration("half"));
        assert!(starts_duration("twenty"));
        assert!(starts_duration("1h30m"));
        assert!(!starts_duration("call"));
        assert!(!starts_duration(""));
    }

    #[test]
    fn borrowed_units() {
        assert_eq!(trailing_unit("1h30m"), 60);
        assert_eq!(trailing_unit("2h30"), 60); // ends in a digit
        assert_eq!(trailing_unit("2h"), 3600);
        assert_eq!(trailing_unit("3d"), 86400);
    }
}
