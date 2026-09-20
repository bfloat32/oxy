//! The calculator: a faithful port of `plugin/Calc.js`.
//!
//! qalc answers almost anything, which is the problem — `qalc -t firefox`
//! returns "0 B" with exit code 0. The gate, the rewrites and the answer
//! shaping are all here because every one of them exists to stop a wrong
//! answer that arrived wearing exit code 0.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

use fancy_regex::Regex;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};

pub struct Calc {
    history: Vec<(String, String)>,
}

impl Default for Calc {
    fn default() -> Self {
        Self::new()
    }
}

impl Calc {
    pub fn new() -> Calc {
        Calc {
            history: load_history(),
        }
    }
}

// --------------------------------------------------------------------- gate
//
// A digit plus either an operator or a conversion word. Both halves matter:
// "5" alone is not a question, and "a + b" is not arithmetic.
fn looks_like_math(text: &str) -> bool {
    if !text.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    if undecidable(text) {
        return false;
    }
    if operator_re().is_match(text).unwrap_or(false) {
        return true;
    }
    if conversion_re().is_match(text).unwrap_or(false) {
        return true;
    }
    money_preposition_re().is_match(text).unwrap_or(false) && names_currency(&with_currencies(text))
}

fn operator_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[+\-*/^%()]").unwrap())
}
fn conversion_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(to|in|into|as|para|pra)\b").unwrap())
}
fn money_preposition_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(em|en)\b").unwrap())
}

// ---------------------------------------------------------------- units
//
// The units qalc reads as something else: `c` is the speed of light, `mb` is
// millibar, and every one is how a person writes the unit they mean.
fn ambiguous_unit(token: &str) -> std::borrow::Cow<'_, str> {
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

// ------------------------------------------------------------------- money

fn symbol_code(sym: &str) -> Option<&'static str> {
    Some(match sym.to_lowercase().as_str() {
        "r$" => "BRL",
        "us$" | "u$s" => "USD",
        "c$" | "ca$" => "CAD",
        "a$" | "au$" => "AUD",
        "nz$" => "NZD",
        "hk$" => "HKD",
        "s$" => "SGD",
        "nt$" => "TWD",
        "mx$" => "MXN",
        "cn¥" => "CNY",
        "₺" => "TRY",
        "₩" => "KRW",
        "₫" => "VND",
        "₴" => "UAH",
        "฿" => "THB",
        "₪" => "ILS",
        _ => return None,
    })
}

fn currency_name(name: &str) -> Option<&'static str> {
    Some(match name.to_lowercase().as_str() {
        "dolar" | "dolares" | "dólar" | "dólares" | "dolari" | "dollari" => "USD",
        "reais" => "BRL",
        "iene" | "ienes" | "yens" | "yenes" => "JPY",
        "esterlina" | "esterlinas" | "sterline" | "sterlina" => "GBP",
        "franc" | "francs" | "franco" | "francos" | "franchi" | "franken" => "CHF",
        "yuan" | "yuanes" | "renminbi" | "rmb" => "CNY",
        "rupee" | "rupees" | "rupia" | "rupias" | "roupie" => "INR",
        "rupiah" => "IDR",
        "shekel" | "shekels" | "sheqel" => "ILS",
        "baht" => "THB",
        "ringgit" => "MYR",
        "dong" => "VND",
        "hryvnia" | "hryvnias" => "UAH",
        "rublo" | "rublos" | "rubel" => "RUB",
        "zloty" | "zlotys" => "PLN",
        "forint" | "forints" => "HUF",
        "rands" => "ZAR",
        "wons" => "KRW",
        "dirham" | "dirhams" => "AED",
        "lira" | "liras" => "TRY",
        _ => return None,
    })
}

fn currency_in_context(name: &str) -> Option<&'static str> {
    Some(match name.to_lowercase().as_str() {
        "pound" | "pounds" | "libra" | "libras" | "livre" | "livres" | "pfund" => "GBP",
        "real" => "BRL",
        _ => return None,
    })
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

fn is_preposition(word: &str) -> bool {
    matches!(
        word.to_lowercase().as_str(),
        "to" | "in" | "into" | "as" | "para" | "pra" | "em" | "en" | "de" | "of"
    )
}

fn is_fiat(token: &str) -> bool {
    matches!(
        token.to_lowercase().as_str(),
        "usd"
            | "eur"
            | "gbp"
            | "jpy"
            | "chf"
            | "cad"
            | "aud"
            | "nzd"
            | "cny"
            | "rmb"
            | "brl"
            | "mxn"
            | "ars"
            | "clp"
            | "cop"
            | "pen"
            | "uyu"
            | "inr"
            | "rub"
            | "krw"
            | "sek"
            | "nok"
            | "dkk"
            | "pln"
            | "czk"
            | "huf"
            | "ron"
            | "try"
            | "zar"
            | "ils"
            | "aed"
            | "sar"
            | "egp"
            | "ngn"
            | "kes"
            | "hkd"
            | "sgd"
            | "twd"
            | "thb"
            | "idr"
            | "myr"
            | "php"
            | "vnd"
            | "isk"
            | "uah"
            | "$"
            | "€"
            | "£"
            | "¥"
            | "₹"
            | "₽"
            | "r$"
            | "us$"
            | "dollar"
            | "dollars"
            | "euro"
            | "euros"
            | "pound"
            | "pounds"
            | "yen"
            | "real"
            | "reais"
            | "peso"
            | "pesos"
            | "rupee"
            | "rupees"
    )
}

fn symbol_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let keys = [
            "r\\$", "us\\$", "u\\$s", "c\\$", "ca\\$", "a\\$", "au\\$", "nz\\$", "hk\\$", "s\\$",
            "nt\\$", "mx\\$", "cn¥", "₺", "₩", "₫", "₴", "฿", "₪",
        ];
        Regex::new(&format!("(^|[^0-9A-Za-z])({})(?![A-Za-z])", keys.join("|")))
            .expect("symbol regex compiles")
    })
}

fn name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let names = [
            "dolari",
            "dollari",
            "dolares",
            "dólares",
            "dolar",
            "dólar",
            "reais",
            "ienes",
            "iene",
            "yenes",
            "yens",
            "esterlinas",
            "esterlina",
            "sterline",
            "sterlina",
            "francos",
            "franchi",
            "franco",
            "francs",
            "franc",
            "franken",
            "yuanes",
            "yuan",
            "renminbi",
            "rmb",
            "rupees",
            "rupee",
            "rupias",
            "rupia",
            "roupie",
            "rupiah",
            "shekels",
            "shekel",
            "sheqel",
            "baht",
            "ringgit",
            "dong",
            "hryvnias",
            "hryvnia",
            "rublos",
            "rublo",
            "rubel",
            "zlotys",
            "zloty",
            "forints",
            "forint",
            "rands",
            "wons",
            "dirhams",
            "dirham",
            "liras",
            "lira",
        ];
        Regex::new(&format!("(?i)\\b({})\\b", names.join("|"))).expect("name regex compiles")
    })
}

fn context_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(pounds|pound|libras|libra|livres|livre|pfund|real)\b")
            .expect("context regex compiles")
    })
}

fn tokens_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[a-z$¥£€₹₽]+").unwrap())
}

fn unit_before_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([A-Za-z$¥£€₹₽₺₩₫₴₪฿À-ɏ]+)\s*$").unwrap())
}
fn unit_after_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*([A-Za-z$¥£€₹₽₺₩₫₴₪฿À-ɏ]+)").unwrap())
}
fn word_after_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s+([A-Za-z$¥£€₹₽₺₩₫₴₪฿À-ɏ]+)(?![A-Za-z])").unwrap())
}
fn target_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\bto\s+([^\s]+)\s*$").unwrap())
}

/// Is any token in here money, ignoring the ones that are only money in
/// company? This is what decides whether `pounds` is sterling or mass.
fn names_currency(text: &str) -> bool {
    // Lowercased first, the way the original read it — the rewrite above
    // emits uppercase codes, and `BRL` is fiat the same as `brl`.
    let t = text.to_lowercase();
    for m in tokens_re().find_iter(&t).map_while(|m| m.ok()) {
        let token = m.as_str();
        if is_fiat(token) && currency_in_context(token).is_none() {
            return true;
        }
        if currency_name(token).is_some() {
            return true;
        }
    }
    false
}

/// Symbols and names to ISO codes, so everything downstream is looking at one
/// spelling of a currency.
fn with_currencies(text: &str) -> String {
    let out = symbol_re()
        .replace_all(text, |caps: &fancy_regex::Captures| {
            let lead = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let sym = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            format!("{}{}", lead, symbol_code(sym).unwrap_or(sym))
        })
        .into_owned();
    let out = name_re()
        .replace_all(&out, |caps: &fancy_regex::Captures| {
            let name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            currency_name(name).unwrap_or(name).to_string()
        })
        .into_owned();
    if !names_currency(&out) {
        return out;
    }
    context_name_re()
        .replace_all(&out, |caps: &fancy_regex::Captures| {
            let name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            currency_in_context(name).unwrap_or(name).to_string()
        })
        .into_owned()
}

// ----------------------------------------------------------------- numbers

struct NumberToken {
    start: usize,
    end: usize,
    text: String,
}

/// Runs of digits and the separators between them. A comma inside parentheses
/// is skipped: `gcd(12,18)` is qalc's own argument separator.
fn number_tokens(text: &str) -> Vec<NumberToken> {
    let s = text.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < s.len() {
        let ch = s[i] as char;
        if ch == '(' {
            depth += 1;
            i += 1;
            continue;
        }
        if ch == ')' {
            if depth > 0 {
                depth -= 1;
            }
            i += 1;
            continue;
        }
        if !ch.is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < s.len() {
            let c = s[i] as char;
            if c.is_ascii_digit() {
                i += 1;
                continue;
            }
            let next_is_digit = i + 1 < s.len() && (s[i + 1] as char).is_ascii_digit();
            if (c == '.' || (c == ',' && depth == 0)) && next_is_digit {
                i += 1;
                continue;
            }
            break;
        }
        out.push(NumberToken {
            start,
            end: i,
            text: text[start..i].to_string(),
        });
    }
    out
}

/// A group is one to three digits and then threes, all the way down.
fn groups_ok(parts: &[&str]) -> bool {
    parts.len() >= 2
        && !parts[0].is_empty()
        && parts[0].len() <= 3
        && parts[1..].iter().all(|p| p.len() == 3)
}

enum Read {
    /// (value, convention: the char this literal proves is this person's
    /// decimal point, if any)
    Value {
        value: String,
        convention: Option<char>,
    },
    Ambiguous {
        sep: char,
        grouped: String,
        decimal: String,
    },
    Bad,
}

/// `{ value }` when the literal can only mean one thing, `ambiguous` for the
/// one shape that means two, `bad` for `1.23.4`, which is no number at all.
fn read_number(token: &str) -> Read {
    let dots = token.matches('.').count();
    let commas = token.matches(',').count();
    if dots == 0 && commas == 0 {
        return Read::Value {
            value: token.to_string(),
            convention: None,
        };
    }

    if dots > 0 && commas > 0 {
        let decimal = if token.rfind('.') > token.rfind(',') {
            '.'
        } else {
            ','
        };
        let group = if decimal == '.' { ',' } else { '.' };
        if token.matches(decimal).count() != 1 {
            return Read::Bad;
        }
        let at = token.rfind(decimal).unwrap();
        let parts: Vec<&str> = token[..at].split(group).collect();
        if !groups_ok(&parts) {
            return Read::Bad;
        }
        return Read::Value {
            value: format!("{}.{}", parts.join(""), &token[at + 1..]),
            convention: Some(decimal),
        };
    }

    let sep = if dots > 0 { '.' } else { ',' };
    let pieces: Vec<&str> = token.split(sep).collect();
    if pieces.len() > 2 {
        if !groups_ok(&pieces) {
            return Read::Bad;
        }
        return Read::Value {
            value: pieces.join(""),
            convention: Some(if sep == '.' { ',' } else { '.' }),
        };
    }

    let whole = pieces[0];
    let frac = pieces[1];
    // Not three digits after it, or more than three in front, or a leading
    // zero: a group cannot be any of those, so the separator is a decimal
    // point. The leading zero is what keeps `0,750 l` at three quarters.
    if frac.len() != 3 || whole.len() > 3 || whole.starts_with('0') {
        return Read::Value {
            value: format!("{whole}.{frac}"),
            convention: Some(sep),
        };
    }
    Read::Ambiguous {
        sep,
        grouped: format!("{whole}{frac}"),
        decimal: format!("{whole}.{frac}"),
    }
}

/// Is this literal a price? The unit written against it decides, and only a
/// currency quoted to two decimals counts: `1.005 btc` is a real quantity.
fn priced_in(text: &str, token: &NumberToken) -> bool {
    if let Ok(Some(caps)) = unit_before_re().captures(&text[..token.start])
        && let Some(word) = caps.get(1)
        && is_fiat(word.as_str())
    {
        return true;
    }

    if let Ok(Some(caps)) = unit_after_re().captures(&text[token.end..])
        && let Some(word) = caps.get(1)
    {
        let word = word.as_str();
        if is_fiat(word) {
            return true;
        }
        if !is_preposition(word) {
            return false;
        }
    }

    if let Ok(Some(caps)) = target_re().captures(text)
        && let Some(target) = caps.get(1)
    {
        return is_fiat(target.as_str());
    }
    false
}

/// Move the decimal point right without arithmetic: the string is the truth
/// here and 1.5 * 1e6 in floating point is not always 1500000.
fn shift_decimal(literal: &str, places: usize) -> String {
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
fn magnitude_after(text: &str, end: usize) -> Option<(usize, usize)> {
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

/// Every literal in plain form, or `None` when one cannot be read with
/// certainty. `None` is the whole point: it is how the calculator declines.
fn with_numbers(text: &str, notes: Option<&mut Vec<String>>) -> Option<String> {
    let tokens = number_tokens(text);
    if tokens.is_empty() {
        return Some(text.to_string());
    }

    let mut reads = Vec::new();
    let mut convention: Option<char> = None;
    for token in &tokens {
        let read = read_number(&token.text);
        match &read {
            Read::Bad => return None,
            Read::Value { convention: c, .. } => {
                if let Some(c) = c {
                    if convention.is_some_and(|have| have != *c) {
                        return None;
                    }
                    convention = Some(*c);
                }
            }
            Read::Ambiguous { .. } => {}
        }
        reads.push(read);
    }

    let scaling = names_currency(text);
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    let mut notes = notes;
    for (i, token) in tokens.iter().enumerate() {
        let value = match &reads[i] {
            Read::Bad => return None,
            Read::Value { value, .. } => value.clone(),
            Read::Ambiguous {
                sep,
                grouped,
                decimal,
            } => {
                let value = if let Some(c) = convention {
                    if *sep == c {
                        decimal.clone()
                    } else {
                        grouped.clone()
                    }
                } else if priced_in(text, token) {
                    grouped.clone()
                } else {
                    return None;
                };
                if let Some(notes) = notes.as_deref_mut()
                    && value != token.text
                {
                    notes.push(format!("{} read as {value}", token.text));
                }
                value
            }
        };
        let magnitude = if scaling {
            magnitude_after(text, token.end)
        } else {
            None
        };
        let value = match magnitude {
            Some((zeros, _)) => shift_decimal(&value, zeros),
            None => value,
        };
        out.push_str(&text[at..token.start]);
        out.push_str(&value);
        at = magnitude.map(|(_, end)| end).unwrap_or(token.end);
    }
    out.push_str(&text[at..]);
    Some(out)
}

/// The one question the gate asks about the digits themselves.
fn undecidable(text: &str) -> bool {
    with_numbers(&with_currencies(text), None).is_none()
}

/// `1.500 usd to eur` is answered as fifteen hundred dollars, and the row says
/// so under the answer. An empty string hides the line.
fn reading(text: &str) -> String {
    let mut notes = Vec::new();
    if with_numbers(&with_currencies(text), Some(&mut notes)).is_none() {
        return String::new();
    }
    notes.join(" · ")
}

/// What to hand qalc. Every rewrite here exists because qalc answered
/// something wrong with exit code 0; none of them touch plain arithmetic.
fn for_qalc(text: &str) -> String {
    let pct_of = Regex::new(r"(?i)%\s+of\s+").unwrap();
    let out = pct_of.replace(text, "% * ").into_owned();

    let out = with_currencies(&out);
    let out = with_numbers(&out, None).unwrap_or(out);

    let into = Regex::new(r"(?i)(\d\s*[^\s]*)\s+into\s+(?=[A-Za-z°])").unwrap();
    let out = into.replace(&out, "$1 to ").into_owned();
    let aspara = Regex::new(r"(?i)(\d\s*[^\s]+)\s+(?:as|para|pra)\s+(?=[A-Za-z°])").unwrap();
    let out = aspara.replace(&out, "$1 to ").into_owned();
    let out = if names_currency(&out) {
        let em = Regex::new(r"(?i)(\d\s*[^\s]+)\s+(?:em|en)\s+(?=[A-Za-z°])").unwrap();
        em.replace(&out, "$1 to ").into_owned()
    } else {
        out
    };
    let inin = Regex::new(r"(?i)(\d\s*[^\s]*)\s+in\s+(?=[A-Za-z°])").unwrap();
    let out = inin.replace(&out, "$1 to ").into_owned();

    let split = Regex::new(r"(?i)^(.*?)\s+to\s+(\S+)\s*$").unwrap();
    let Ok(Some(caps)) = split.captures(&out) else {
        return out;
    };
    let left = caps.get(1).unwrap().as_str();
    let right = caps.get(2).unwrap().as_str();

    let unit_at_end = Regex::new(r"([\d.]\s*)([A-Za-z°]+)\s*$").unwrap();
    let left = unit_at_end
        .replace(left, |caps: &fancy_regex::Captures| {
            let num = caps.get(1).unwrap().as_str();
            let unit = caps.get(2).unwrap().as_str();
            format!("{}{}", num, ambiguous_unit(unit))
        })
        .into_owned();
    format!("{} to {}", left, ambiguous_unit(right))
}

// --------------------------------------------------------------- formatting

const PRECISION_FLOOR: usize = 6;
const PRECISION_CEILING: usize = 20;
const EXPONENT_FROM: usize = 21;

fn significant_digits(literal: &str) -> usize {
    literal
        .chars()
        .filter(|c| c.is_ascii_digit())
        .skip_while(|c| *c == '0')
        .count()
}

fn precision_for(text: &str) -> usize {
    let lit_re = Regex::new(r"\d+(?:\.\d+)?").unwrap();
    let mut want = PRECISION_FLOOR;
    for m in lit_re.find_iter(text).map_while(|m| m.ok()) {
        want = want.max(significant_digits(m.as_str()));
    }
    want.min(PRECISION_CEILING)
}

/// Money, and specifically money the answer will be *in*: the right-hand side
/// of a conversion decides.
fn is_money(text: &str) -> bool {
    let t = text.to_lowercase();
    if let Ok(Some(caps)) = target_re().captures(&t) {
        return is_fiat(caps.get(1).unwrap().as_str());
    }
    tokens_re()
        .find_iter(&t)
        .map_while(|m| m.ok())
        .any(|m| is_fiat(m.as_str()))
}

/// The whole call: what qalc is asked and how the answer is written are one
/// decision. -m bounds a pathological expression.
fn command(text: &str) -> Vec<String> {
    let expression = for_qalc(text);
    let mut argv = vec![
        "qalc".to_string(),
        "-t".to_string(),
        "-m".to_string(),
        "200".to_string(),
        "-set".to_string(),
        format!("exp {EXPONENT_FROM}"),
        "-set".to_string(),
        format!("precision {}", precision_for(&expression)),
    ];
    if is_money(&expression) {
        argv.extend(["-set".into(), "maxdeci 2".into()]);
    }
    if Regex::new(r"(?i)\bto\b")
        .unwrap()
        .is_match(&expression)
        .unwrap_or(false)
    {
        argv.extend(["-set".into(), "conv 0".into()]);
    }
    argv.push("--".into());
    argv.push(expression);
    argv
}

fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_echo(query: &str, answer: &str) -> bool {
    normalize(query).eq_ignore_ascii_case(&normalize(answer))
}

/// Parse qalc's stdout into a row. `None` is a refusal: an echo, or an
/// undecidable number nobody can read.
fn parse(query: &str, stdout: &str) -> Option<String> {
    let raw = normalize(stdout);
    if raw.is_empty() || is_echo(query, &raw) {
        return None;
    }
    if undecidable(query) {
        return None;
    }
    // qalc writes a U+2212 in an exponent; nothing that reads the answer back
    // knows it. Unicode stays on for μs and Ωs; this one character goes.
    Some(raw.replace('−', "-"))
}

// ------------------------------------------------------------- the provider

impl NativeExt for Calc {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move {
            let expression = ctx.arg.clone();
            // Scoped (`=`/`calc:`) skips the gate: you asked, so you get qalc's
            // reading whatever it is. Unscoped, the gate decides.
            if ctx.query.scope.is_empty() && !looks_like_math(&expression) {
                return NativeOutcome::Empty;
            }
            if expression.is_empty() {
                return NativeOutcome::Empty;
            }

            // The placeholder first: Enter must not fall through to an app
            // while qalc is thinking.
            let _ = progress.send(vec![json!({
                "id": expression,
                "title": expression,
                "subtitle": "calculating",
                "accessory": "",
                "pending": true,
                "view": "hero",
            })]);

            let argv = command(&expression);
            let output = tokio::process::Command::new(&argv[0])
                .args(&argv[1..])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .await;
            let Ok(output) = output else {
                return NativeOutcome::Empty;
            };
            let stdout = String::from_utf8_lossy(&output.stdout);

            let Some(answer) = parse(&expression, &stdout) else {
                return NativeOutcome::Empty;
            };

            // Copying the answer and remembering it are one gesture. `calc:`
            // reads the file this writes, and its `when` is a test that the
            // file has anything in it.
            let record = format!(
                "oxy-calc-history record {} {}",
                crate::support::quote::quote(&expression),
                crate::support::quote::quote(&answer)
            );
            let detail = reading(&expression);
            let mut row = json!({
                "id": expression,
                "title": answer,
                "subtitle": expression,
                "detail": detail,
                "accessory": "Copy",
                "exec": record,
                "view": "hero",
                "score": 90000,
                "actions": [
                    { "title": "Copy Result", "shortcut": "↵", "exec": record },
                    { "title": "Copy Expression", "exec": format!(
                        "printf %s {} | wl-copy",
                        crate::support::quote::quote(&expression)) },
                ],
            });
            if detail.is_empty() {
                row.as_object_mut().unwrap().remove("detail");
            }
            NativeOutcome::Rows(vec![row])
        })
    }
}

// Accepted answers, so `calc:` has something to recall. The file is the one
// `oxy-calc-history` writes; this provider records to it directly.
fn load_history() -> Vec<(String, String)> {
    let path = crate::settings::paths::calc_history_file();
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| {
            Some((
                e.get("expression")?.as_str()?.to_string(),
                e.get("answer")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

#[allow(dead_code)]
impl Calc {
    fn history(&self) -> &[(String, String)] {
        &self.history
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate() {
        assert!(looks_like_math("2+2"));
        assert!(looks_like_math("5 km to miles"));
        assert!(!looks_like_math("firefox"));
        assert!(!looks_like_math("5"));
    }

    #[test]
    fn numbers() {
        // Both conventions resolve; the ambiguous lone shape declines.
        assert_eq!(with_numbers("1.500,50 brl", None).unwrap(), "1500.50 brl");
        assert_eq!(with_numbers("1.500 kg", None), None);
        assert_eq!(with_numbers("100,000 yen", None).unwrap(), "100000 yen");
        assert_eq!(with_numbers("1.23.4", None), None);
    }

    #[test]
    fn conversions_rewrite() {
        assert_eq!(for_qalc("40 miles in km"), "40 miles to km");
        assert_eq!(for_qalc("3 in"), "3 in"); // a quantity, not a conversion
        // Codes pass through as written — qalc reads `brl` and `BRL` alike;
        // only the ambiguous units get a canonical spelling.
        assert_eq!(for_qalc("100 usd into brl"), "100 usd to brl");
        assert!(for_qalc("100 reais em dolares").ends_with("to USD"));
    }

    #[test]
    fn echo_is_refused() {
        assert_eq!(parse("firefox", "firefox"), None);
        assert_eq!(parse("2+2", "4"), Some("4".to_string()));
    }
}
