use std::sync::OnceLock;

use fancy_regex::Regex;

// ---------------------------------------------------------------- units
//
// The units qalc reads as something else: `c` is the speed of light, `mb` is
// millibar, and every one is how a person writes the unit they mean.
pub(super) fn ambiguous_unit(token: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    let canonical: &'static str = match token.to_lowercase().as_str() {
        "f" | "°f" | "degf" | "fahrenheit" => "°F",
        "c" | "°c" | "degc" | "celsius" => "°C",
        "k" | "kelvin" => "K",
        "kb" => "kilobyte",
        "mb" => "megabyte",
        "gb" => "gigabyte",
        "tb" => "terabyte",
        "pb" => "petabyte",
        "kib" => "kibibyte",
        "mib" => "mebibyte",
        "gib" => "gibibyte",
        "tib" => "tebibyte",
        "kbit" => "kilobit",
        "mbit" => "megabit",
        "gbit" => "gigabit",
        "in" => "inch",
        _ => return Cow::Borrowed(token),
    };
    Cow::Borrowed(canonical)
}

// [suffix, zeros] — checked in order, longest first.
const MAGNITUDE_SUFFIX: &[(&str, usize)] = &[
    ("mm", 6),
    ("mn", 6),
    ("mi", 6),
    ("bn", 9),
    ("bi", 9),
    ("m", 6),
    ("b", 9),
];

fn magnitude_word(word: &str) -> Option<usize> {
    Some(match word.to_lowercase().as_str() {
        "mil" => 3,
        "mi" | "mn" | "mio" | "mln" => 6,
        "milhao" | "milhão" | "milhoes" | "milhões" => 6,
        "millon" | "millón" | "millones" | "milione" | "milioni" => 6,
        "bi" | "bn" | "bilhao" | "bilhão" | "bilhoes" | "bilhões" => 9,
        "billon" | "billón" | "billones" | "miliardo" | "miliardi" => 9,
        _ => return None,
    })
}

pub(super) fn unit_before_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([A-Za-z$¥£€₹₽₺₩₫₴₪฿À-ɏ]+)\s*$").unwrap())
}
pub(super) fn unit_after_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*([A-Za-z$¥£€₹₽₺₩₫₴₪฿À-ɏ]+)").unwrap())
}
fn word_after_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s+([A-Za-z$¥£€₹₽₺₩₫₴₪฿À-ɏ]+)(?![A-Za-z])").unwrap())
}

/// Move the decimal point right without arithmetic: the string is the truth
/// here and 1.5 * 1e6 in floating point is not always 1500000.
pub(super) fn shift_decimal(literal: &str, places: usize) -> String {
    let (mut whole, mut frac) = match literal.find('.') {
        Some(dot) => (literal[..dot].to_string(), literal[dot + 1..].to_string()),
        None => (literal.to_string(), String::new()),
    };
    for _ in 0..places {
        if frac.is_empty() {
            whole.push('0');
        } else {
            whole.push(frac.remove(0));
        }
    }
    if frac.is_empty() {
        whole
    } else {
        format!("{whole}.{frac}")
    }
}

/// `1.5m`, `2bn`, `100 mil`, `1.5 milhoes de`: how many zeros, and where the
/// text picks up again. Only called when the query names a currency.
pub(super) fn magnitude_after(text: &str, end: usize) -> Option<(usize, usize)> {
    let rest = &text[end..];
    for (suffix, zeros) in MAGNITUDE_SUFFIX {
        if rest.len() >= suffix.len()
            && rest[..suffix.len()].eq_ignore_ascii_case(suffix)
            && !rest[suffix.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            return Some((*zeros, end + suffix.len()));
        }
    }
    if let Ok(Some(caps)) = word_after_re().captures(rest) {
        let word = caps.get(1).unwrap().as_str();
        if let Some(zeros) = magnitude_word(word) {
            let whole = caps.get(0).unwrap().as_str();
            let tail = &rest[whole.len()..];
            // `1.5 milhoes de reais`: the preposition belongs to the number
            // word, and qalc has no use for it.
            let mut end = end + whole.len();
            if let Ok(Some(t)) = Regex::new(r"(?i)^\s+(de|of)(?![A-Za-z])")
                .unwrap()
                .find(tail)
            {
                end += t.end();
            }
            return Some((zeros, end));
        }
    }
    None
}
