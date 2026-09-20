//! Fuzzy scoring for anything shaped like a desktop entry:
//! `{ id, name, genericName, comment, keywords }`.
//!
//! A port of `plugin/Score.js`, which is itself Omarchy's `AppSearch.js`
//! scoring copied rather than imported — apps, commands, actions and
//! quicklinks are ranked by one function so the numbers mean the same thing.
//!
//! The bands are the contract: 10000 name prefix, 9500 id prefix, 8000 name
//! infix, 7600 id infix, 6000 any haystack hit, 5000/4600 acronym,
//! 4000 fallback. `rank::tier_for_fuzzy` maps them onto tiers.

use std::sync::OnceLock;

use fancy_regex::Regex;

/// A thing that can be searched: an app, a command, an action, a quicklink.
#[derive(Debug, Clone, Default)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub generic_name: String,
    pub comment: String,
    pub keywords: Vec<String>,
    /// The row the match becomes, carried so scoring needs no second lookup.
    pub payload: serde_json::Value,
}

impl Entry {
    fn entry_name(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }

    fn keyword_text(&self) -> String {
        self.keywords.join(" ")
    }

    /// Everything the haystack band searches, lowercased.
    fn search_text(&self) -> String {
        format!(
            "{} {} {} {} {}",
            self.name,
            self.generic_name,
            self.comment,
            self.keyword_text(),
            self.id
        )
        .to_lowercase()
    }
}

fn camel_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([a-z0-9])([A-Z])").expect("camel regex compiles"))
}

fn separators_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[._:/\\-]+").expect("separator regex compiles"))
}

fn word_text(value: &str) -> String {
    let expanded = camel_re().replace_all(value, "${1} ${2}");
    separators_re().replace_all(&expanded, " ").to_lowercase()
}

fn words(value: &str) -> Vec<String> {
    word_text(value)
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect()
}

/// First letters of every word in name, generic, keywords and id — what an
/// acronym like `ff` matches against.
fn acronym(entry: &Entry) -> String {
    let joined = format!(
        "{} {} {} {}",
        entry.name,
        entry.generic_name,
        entry.keyword_text(),
        entry.id
    );
    words(&joined)
        .iter()
        .filter_map(|w| w.chars().next())
        .collect()
}

fn term_matches(entry: &Entry, search_text: &str, acronym: &str, term: &str) -> bool {
    if term.is_empty() {
        return true;
    }
    let name = entry.entry_name().to_lowercase();
    let id = entry.id.to_lowercase();

    if name.contains(term) || id.contains(term) || search_text.contains(term) {
        return true;
    }
    // An acronym match on a long term is almost always noise.
    term.len() <= 5 && acronym.contains(term)
}

fn all_terms_match(entry: &Entry, search_text: &str, acronym: &str, query: &str) -> bool {
    query
        .split_whitespace()
        .all(|term| term_matches(entry, search_text, acronym, term))
}

/// `-1` when the entry does not match at all.
pub fn fuzzy(entry: &Entry, query: &str) -> i64 {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return 0;
    }
    let search_text = entry.search_text();
    let acro = acronym(entry);
    if !all_terms_match(entry, &search_text, &acro, &q) {
        return -1;
    }

    let name = entry.entry_name().to_lowercase();
    let id = entry.id.to_lowercase();
    // `String.length` is UTF-16 units — the band lengths the script build
    // scored against, so a non-ASCII name orders identically on both builds.
    let name_len = name.encode_utf16().count() as i64;
    let id_len = id.encode_utf16().count() as i64;

    // Prefix before infix, name before id — the order Score.js checks, so an
    // id-prefix match outranks a name that merely contains the query.
    if name.starts_with(&q) {
        return 10000 - name_len;
    }
    if id.starts_with(&q) {
        return 9500 - id_len;
    }
    if let Some(at) = name.find(&q) {
        return 8000 - at as i64 * 10 - name_len;
    }
    if let Some(at) = id.find(&q) {
        return 7600 - at as i64 * 10 - id_len;
    }

    if let Some(at) = search_text.find(&q) {
        return 6000 - at as i64;
    }

    if let Some(at) = acro.find(&q) {
        if at == 0 {
            return 5000 - acro.encode_utf16().count() as i64;
        }
        return 4600 - at as i64 * 10 - acro.encode_utf16().count() as i64;
    }

    4000 - name_len
}

#[cfg(test)]
mod tests {
    use super::*;

    fn firefox() -> Entry {
        Entry {
            id: "firefox.desktop".into(),
            name: "Firefox".into(),
            generic_name: "Web Browser".into(),
            comment: "Browse the web".into(),
            keywords: vec!["internet".into(), "mozilla".into()],
            payload: serde_json::Value::Null,
        }
    }

    #[test]
    fn prefix_beats_infix() {
        let e = firefox();
        assert!(fuzzy(&e, "fire") > fuzzy(&e, "oxfo"));
        assert!(fuzzy(&e, "fire") >= 9500);
    }

    #[test]
    fn no_match_is_negative() {
        assert!(fuzzy(&firefox(), "zzzzz") < 0);
    }

    #[test]
    fn keyword_hits() {
        let e = firefox();
        assert!(fuzzy(&e, "mozilla") >= 4000);
    }

    #[test]
    fn empty_query_is_neutral() {
        assert_eq!(fuzzy(&firefox(), ""), 0);
    }
}
