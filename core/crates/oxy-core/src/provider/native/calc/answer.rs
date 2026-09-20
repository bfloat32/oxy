use fancy_regex::Regex;
use serde_json::Value;

use super::Calc;
use super::money::{names_currency, with_currencies};
use super::numbers::with_numbers;
use super::units::ambiguous_unit;

/// The one question the gate asks about the digits themselves.
pub(super) fn undecidable(text: &str) -> bool {
    with_numbers(&with_currencies(text), None).is_none()
}

/// `1.500 usd to eur` is answered as fifteen hundred dollars, and the row says
/// so under the answer. An empty string hides the line.
pub(super) fn reading(text: &str) -> String {
    let mut notes = Vec::new();
    if with_numbers(&with_currencies(text), Some(&mut notes)).is_none() {
        return String::new();
    }
    notes.join(" · ")
}

/// What to hand qalc. Every rewrite here exists because qalc answered
/// something wrong with exit code 0; none of them touch plain arithmetic.
pub(super) fn for_qalc(text: &str) -> String {
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

fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_echo(query: &str, answer: &str) -> bool {
    normalize(query).eq_ignore_ascii_case(&normalize(answer))
}

/// Parse qalc's stdout into a row. `None` is a refusal: an echo, or an
/// undecidable number nobody can read.
pub(super) fn parse(query: &str, stdout: &str) -> Option<String> {
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

// Accepted answers, so `calc:` has something to recall. The file is the one
// `oxy-calc-history` writes; this provider records to it directly.
pub(super) fn load_history() -> Vec<(String, String)> {
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
