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

/// The match position as the script sees it: `String.prototype.indexOf`
/// counts UTF-16 code units, `str::find` counts bytes. Every band below is
/// arithmetic on that position, so without this a name with an accent before
/// the match scores ten points lower per extra byte — the same length trap
/// `name_len` already avoids, one line further down.
fn utf16_at(text: &str, byte_at: usize) -> i64 {
    text[..byte_at].encode_utf16().count() as i64
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
        return 8000 - utf16_at(&name, at) * 10 - name_len;
    }
    if let Some(at) = id.find(&q) {
        return 7600 - utf16_at(&id, at) * 10 - id_len;
    }

    if let Some(at) = search_text.find(&q) {
        return 6000 - utf16_at(&search_text, at);
    }

    if let Some(at) = acro.find(&q) {
        let acro_len = acro.encode_utf16().count() as i64;
        if at == 0 {
            return 5000 - acro_len;
        }
        return 4600 - utf16_at(&acro, at) * 10 - acro_len;
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

    fn entry(id: &str, name: &str, generic: &str, comment: &str, keywords: &[&str]) -> Entry {
        Entry {
            id: id.into(),
            name: name.into(),
            generic_name: generic.into(),
            comment: comment.into(),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            payload: serde_json::Value::Null,
        }
    }

    /// The port checked against the build it is a port of: every expected
    /// score below was produced by running `plugin/Score.js` under node over
    /// these same entries and queries. A failure here means the two builds
    /// have drifted — most easily on a non-ASCII name, where a byte offset
    /// and a UTF-16 index are different numbers.
    #[test]
    fn scores_match_the_script() {
        let entries = [
            entry(
                "firefox.desktop",
                "Firefox",
                "Web Browser",
                "Browse the web",
                &["internet", "mozilla"],
            ),
            entry(
                "bucher.desktop",
                "Bücher",
                "Library",
                "Read books",
                &["ebook"],
            ),
            entry(
                "eclair.desktop",
                "Éclair",
                "Dessert",
                "Un gâteau",
                &["sweet"],
            ),
            entry(
                "code.desktop",
                "Visual Studio Code",
                "Editor",
                "Code editor",
                &["vscode", "ide"],
            ),
            entry(
                "org.gnome.Nautilus.desktop",
                "Files",
                "File Manager",
                "Browse your files",
                &["nautilus"],
            ),
            entry("übersicht.desktop", "Übersicht", "", "", &[]),
        ];
        #[rustfmt::skip]
        let cases: &[(usize, &str, i64)] = &[
            (0, "fire", 9993),
            (0, "firefox", 9993),
            (0, "oxfo", -1),
            (0, "FIRE", 9993),
            (0, "browse", 5988),
            (0, "mozilla", 5956),
            (0, "vscode", -1),
            (0, "vsc", -1),
            (0, "cher", -1),
            (0, "bücher", -1),
            (0, "büc", -1),
            (0, "buch", -1),
            (0, "clair", -1),
            (0, "éclair", -1),
            (0, "gâteau", -1),
            (0, "ecl", -1),
            (0, "files", -1),
            (0, "file manager", -1),
            (0, "nautilus", -1),
            (0, "über", -1),
            (0, "bersicht", -1),
            (0, "zzz", -1),
            (0, "visual code", -1),
            (0, "studio", -1),
            (0, "code", -1),
            (0, "desktop", 7505),
            (1, "fire", -1),
            (1, "firefox", -1),
            (1, "oxfo", -1),
            (1, "FIRE", -1),
            (1, "browse", -1),
            (1, "mozilla", -1),
            (1, "vscode", -1),
            (1, "vsc", -1),
            (1, "cher", 7974),
            (1, "bücher", 9994),
            (1, "büc", 9994),
            (1, "buch", 9486),
            (1, "clair", -1),
            (1, "éclair", -1),
            (1, "gâteau", -1),
            (1, "ecl", -1),
            (1, "files", -1),
            (1, "file manager", -1),
            (1, "nautilus", -1),
            (1, "über", -1),
            (1, "bersicht", -1),
            (1, "zzz", -1),
            (1, "visual code", -1),
            (1, "studio", -1),
            (1, "code", -1),
            (1, "desktop", 7516),
            (2, "fire", -1),
            (2, "firefox", -1),
            (2, "oxfo", -1),
            (2, "FIRE", -1),
            (2, "browse", -1),
            (2, "mozilla", -1),
            (2, "vscode", -1),
            (2, "vsc", -1),
            (2, "cher", -1),
            (2, "bücher", -1),
            (2, "büc", -1),
            (2, "buch", -1),
            (2, "clair", 7984),
            (2, "éclair", 9994),
            (2, "gâteau", 5982),
            (2, "ecl", 9486),
            (2, "files", -1),
            (2, "file manager", -1),
            (2, "nautilus", -1),
            (2, "über", -1),
            (2, "bersicht", -1),
            (2, "zzz", -1),
            (2, "visual code", -1),
            (2, "studio", -1),
            (2, "code", -1),
            (2, "desktop", 7516),
            (3, "fire", -1),
            (3, "firefox", -1),
            (3, "oxfo", -1),
            (3, "FIRE", -1),
            (3, "browse", -1),
            (3, "mozilla", -1),
            (3, "vscode", 5962),
            (3, "vsc", 5962),
            (3, "cher", -1),
            (3, "bücher", -1),
            (3, "büc", -1),
            (3, "buch", -1),
            (3, "clair", -1),
            (3, "éclair", -1),
            (3, "gâteau", -1),
            (3, "ecl", -1),
            (3, "files", -1),
            (3, "file manager", -1),
            (3, "nautilus", -1),
            (3, "über", -1),
            (3, "bersicht", -1),
            (3, "zzz", -1),
            (3, "visual code", 3982),
            (3, "studio", 7912),
            (3, "code", 9488),
            (3, "desktop", 7538),
            (4, "fire", -1),
            (4, "firefox", -1),
            (4, "oxfo", -1),
            (4, "FIRE", -1),
            (4, "browse", 5981),
            (4, "mozilla", -1),
            (4, "vscode", -1),
            (4, "vsc", -1),
            (4, "cher", -1),
            (4, "bücher", -1),
            (4, "büc", -1),
            (4, "buch", -1),
            (4, "clair", -1),
            (4, "éclair", -1),
            (4, "gâteau", -1),
            (4, "ecl", -1),
            (4, "files", 9995),
            (4, "file manager", 5994),
            (4, "nautilus", 7474),
            (4, "über", -1),
            (4, "bersicht", -1),
            (4, "zzz", -1),
            (4, "visual code", -1),
            (4, "studio", -1),
            (4, "code", -1),
            (4, "desktop", 7384),
            (5, "fire", -1),
            (5, "firefox", -1),
            (5, "oxfo", -1),
            (5, "FIRE", -1),
            (5, "browse", -1),
            (5, "mozilla", -1),
            (5, "vscode", -1),
            (5, "vsc", -1),
            (5, "cher", -1),
            (5, "bücher", -1),
            (5, "büc", -1),
            (5, "buch", -1),
            (5, "clair", -1),
            (5, "éclair", -1),
            (5, "gâteau", -1),
            (5, "ecl", -1),
            (5, "files", -1),
            (5, "file manager", -1),
            (5, "nautilus", -1),
            (5, "über", 9991),
            (5, "bersicht", 7981),
            (5, "zzz", -1),
            (5, "visual code", -1),
            (5, "studio", -1),
            (5, "code", -1),
            (5, "desktop", 7483),
        ];
        for (i, q, want) in cases {
            assert_eq!(
                fuzzy(&entries[*i], q),
                *want,
                "entry {:?} query {:?}",
                entries[*i].id,
                q
            );
        }
    }
}
