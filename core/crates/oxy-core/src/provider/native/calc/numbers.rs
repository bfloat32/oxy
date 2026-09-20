use fancy_regex::Regex;

use super::money::{names_currency, priced_in};
use super::units::{magnitude_after, shift_decimal};

// ----------------------------------------------------------------- numbers

pub(super) struct NumberToken {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) text: String,
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

/// Every literal in plain form, or `None` when one cannot be read with
/// certainty. `None` is the whole point: it is how the calculator declines.
pub(super) fn with_numbers(text: &str, notes: Option<&mut Vec<String>>) -> Option<String> {
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

// --------------------------------------------------------------- formatting

const PRECISION_FLOOR: usize = 6;
const PRECISION_CEILING: usize = 20;
pub(super) const EXPONENT_FROM: usize = 21;

fn significant_digits(literal: &str) -> usize {
    literal
        .chars()
        .filter(|c| c.is_ascii_digit())
        .skip_while(|c| *c == '0')
        .count()
}

pub(super) fn precision_for(text: &str) -> usize {
    let lit_re = Regex::new(r"\d+(?:\.\d+)?").unwrap();
    let mut want = PRECISION_FLOOR;
    for m in lit_re.find_iter(text).map_while(|m| m.ok()) {
        want = want.max(significant_digits(m.as_str()));
    }
    want.min(PRECISION_CEILING)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        // Both conventions resolve; the ambiguous lone shape declines.
        assert_eq!(with_numbers("1.500,50 brl", None).unwrap(), "1500.50 brl");
        assert_eq!(with_numbers("1.500 kg", None), None);
        assert_eq!(with_numbers("100,000 yen", None).unwrap(), "100000 yen");
        assert_eq!(with_numbers("1.23.4", None), None);
    }
}
