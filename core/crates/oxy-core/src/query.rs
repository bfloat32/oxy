//! The box's text, parsed. A port of `plugin/Query.js`.
//!
//! A query is free text plus `keyword:value` filters:
//!
//! ```text
//! firefox                       text only
//! file:report.pdf               a filter, and no text
//! music:"kind of blue" jazz     a quoted value, and text beside it
//! =2+2                          a sigil, shorthand for calc:2+2
//! ```

use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

use fancy_regex::Regex;

/// One-character shorthands for the filters people reach for most.
pub fn sigil(keyword_for: char) -> Option<&'static str> {
    match keyword_for {
        '=' => Some("calc"),
        '>' => Some("run"),
        '?' => Some("web"),
        '/' => Some("command"),
        _ => None,
    }
}

/// Every sigil character that names a keyword, for the `?` listing.
pub fn sigils_for(keyword: &str) -> Vec<char> {
    ['=', '>', '?', '/']
        .into_iter()
        .filter(|c| sigil(*c) == Some(keyword))
        .collect()
}

/// `/` means a command; a path is the one thing that genuinely starts with a
/// slash, so a rest that looks like a path goes to files instead.
fn looks_like_path(rest: &str) -> bool {
    rest.contains('/') || rest.contains('.') || rest.contains('~')
}

fn filter_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"([a-z][a-z0-9_-]*):(?:"([^"]*)"|'([^']*)'|(\S*))"#)
            .expect("filter regex compiles")
    })
}

#[derive(Debug, Clone)]
pub struct Query {
    pub epoch: u64,
    pub raw: String,
    /// The free text with every filter removed.
    pub text: String,
    pub lower: String,
    /// Filter name → value, for every *known* filter found.
    pub filters: BTreeMap<String, String>,
    /// The first filter named. Providers that are not it stay quiet.
    pub scope: String,
    pub empty: bool,
}

impl Query {
    /// `known` is every keyword and alias anything has registered. A `word:`
    /// outside it stays literal text, which is what keeps `https://x` from
    /// being read as an `https` filter. `None` accepts everything — the first
    /// keystroke after a reload, before the registry has spoken.
    pub fn parse(raw: &str, epoch: u64, known: Option<&HashSet<String>>) -> Query {
        let trimmed = raw.trim_start();

        // A leading sigil is rewritten into its filter, so everything
        // downstream sees one shape rather than two.
        if let Some(first) = trimmed.chars().next() {
            if let Some(keyword) = sigil(first) {
                let rest = trimmed[first.len_utf8()..].trim().to_string();
                let keyword = if keyword == "command" && looks_like_path(&rest) {
                    "file"
                } else {
                    keyword
                };
                let mut filters = BTreeMap::new();
                filters.insert(keyword.to_string(), rest);
                return Query::build(raw, "", filters, vec![keyword.to_string()], epoch);
            }
        }

        let mut filters = BTreeMap::new();
        let mut order: Vec<String> = Vec::new();
        let is_known = |key: &str| known.is_none() || known.is_some_and(|k| k.contains(key));

        // Pull the filters out; whatever is left is the free text. Matches of
        // names nobody registered stay where they were.
        let mut rest = String::with_capacity(raw.len());
        let mut at = 0usize;
        for caps in filter_re().captures_iter(raw).map_while(|c| c.ok()) {
            let whole = caps.get(0).expect("group 0");
            let key = whole
                .as_str()
                .split(':')
                .next()
                .unwrap_or("")
                .to_lowercase();
            if !is_known(&key) {
                continue;
            }
            let value = caps
                .get(2)
                .or_else(|| caps.get(3))
                .or_else(|| caps.get(4))
                .map(|m| m.as_str())
                .unwrap_or("");
            rest.push_str(&raw[at..whole.start()]);
            rest.push(' ');
            at = whole.end();
            filters.insert(key.clone(), value.to_string());
            if !order.contains(&key) {
                order.push(key);
            }
        }
        rest.push_str(&raw[at..]);

        Query::build(raw, &rest, filters, order, epoch)
    }

    fn build(
        raw: &str,
        rest: &str,
        filters: BTreeMap<String, String>,
        order: Vec<String>,
        epoch: u64,
    ) -> Query {
        let text = rest.split_whitespace().collect::<Vec<_>>().join(" ");
        Query {
            epoch,
            raw: raw.to_string(),
            lower: text.to_lowercase(),
            text,
            filters,
            scope: order.first().cloned().unwrap_or_default(),
            empty: rest.trim().is_empty() && order.is_empty(),
        }
    }

    /// Should this provider answer? Unscoped, everyone who can answer does.
    /// Scoped, only the named provider, plus anything that declared the same
    /// keyword as an alias.
    pub fn routes_to(&self, provider_id: &str, aliases: &[String]) -> bool {
        if self.scope.is_empty() || self.scope == provider_id {
            return true;
        }
        aliases.iter().any(|a| *a == self.scope)
    }

    /// The text a scoped provider should search: the filter's own value when
    /// it has one, and the free text otherwise. `file:report budget` searches
    /// for "report budget".
    pub fn arg_for(&self, provider_id: &str, aliases: &[String]) -> String {
        let mut value = self.filters.get(provider_id);
        if value.is_none() {
            value = aliases.iter().find_map(|a| self.filters.get(a));
        }
        match value {
            None => self.text.clone(),
            Some(v) if v.is_empty() => self.text.clone(),
            Some(v) if self.text.is_empty() => v.clone(),
            Some(v) => format!("{v} {}", self.text),
        }
    }

    /// Filters nobody claimed, read as extra conditions:
    /// `file:report format:pdf` reaches the file provider with format in hand.
    pub fn extras(&self, provider_id: &str, aliases: &[String]) -> BTreeMap<String, String> {
        self.filters
            .iter()
            .filter(|(k, _)| *k != provider_id && !aliases.contains(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> HashSet<String> {
        ["file", "format", "in", "music", "year"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn plain_text() {
        let q = Query::parse("firefox", 1, Some(&known()));
        assert_eq!(q.text, "firefox");
        assert_eq!(q.scope, "");
        assert!(!q.empty);
    }

    #[test]
    fn filter_only() {
        let q = Query::parse("file:report.pdf", 1, Some(&known()));
        assert_eq!(q.scope, "file");
        assert_eq!(q.filters["file"], "report.pdf");
        assert_eq!(q.text, "");
    }

    #[test]
    fn quoted_value_with_text() {
        let q = Query::parse("music:\"kind of blue\" jazz", 1, Some(&known()));
        assert_eq!(q.filters["music"], "kind of blue");
        assert_eq!(q.text, "jazz");
    }

    #[test]
    fn unknown_keyword_stays_text() {
        let q = Query::parse("https://example.com", 1, Some(&known()));
        assert_eq!(q.scope, "");
        assert_eq!(q.text, "https://example.com");
    }

    #[test]
    fn sigils() {
        let q = Query::parse("=2+2", 1, Some(&known()));
        assert_eq!(q.scope, "calc");
        assert_eq!(q.filters["calc"], "2+2");

        let q = Query::parse("total =2+2", 1, Some(&known()));
        assert_eq!(q.scope, "");
        assert_eq!(q.text, "total =2+2");
    }

    #[test]
    fn slash_path_is_file() {
        let q = Query::parse("/~/Documents", 1, Some(&known()));
        assert_eq!(q.scope, "file");
        let q = Query::parse("/etc", 1, Some(&known()));
        assert_eq!(q.scope, "command");
        assert_eq!(q.filters["command"], "etc");
    }

    #[test]
    fn arg_for_scoped() {
        let q = Query::parse("file:report budget", 1, Some(&known()));
        assert_eq!(q.arg_for("file", &[]), "report budget");
        let q = Query::parse("file:", 1, Some(&known()));
        assert_eq!(q.arg_for("file", &[]), "");
    }

    #[test]
    fn extras() {
        let q = Query::parse("file:report format:pdf in:~/Sync", 1, Some(&known()));
        let ex = q.extras("file", &[]);
        assert_eq!(ex["format"], "pdf");
        assert_eq!(ex["in"], "~/Sync");
    }

    #[test]
    fn bare_filter_keeps_empty_value() {
        let q = Query::parse("win:", 1, None);
        assert_eq!(q.scope, "win");
        assert_eq!(q.filters["win"], "");
    }
}
