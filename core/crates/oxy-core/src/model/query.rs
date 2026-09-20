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
        // `(?i)` is the script's `/i`: `FILE:x` is a filter, and the key is
        // lowercased after the match. Without it an upper-case keyword is
        // read as plain text and the provider it named never hears about it.
        Regex::new(r#"(?i)([a-z][a-z0-9_-]*):(?:"([^"]*)"|'([^']*)'|(\S*))"#)
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
        if let Some(first) = trimmed.chars().next()
            && let Some(keyword) = sigil(first)
        {
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
        aliases.contains(&self.scope)
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

    /// The port checked against the build it is a port of: every field below
    /// was produced by running `plugin/Query.js` under node over the same raw
    /// strings and the same registered keywords. A failure means the two
    /// parsers disagree about what a keystroke means.
    #[test]
    fn parsing_matches_the_script() {
        struct Case {
            raw: &'static str,
            text: &'static str,
            scope: &'static str,
            empty: bool,
            filters: &'static [(&'static str, &'static str)],
            arg_file: &'static str,
            extras_file: &'static [(&'static str, &'static str)],
            routes_file: bool,
            routes_apps: bool,
        }
        let known: HashSet<String> = [
            "calc", "run", "web", "command", "files", "file", "apps", "app", "win", "windows",
            "git", "tz", "timezone", "format", "in", "type", "music", "sp", "emoji",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let aliases_file = vec!["files".to_string()];
        let aliases_apps = vec!["app".to_string()];

        #[rustfmt::skip]
        let cases: &[Case] = &[
            Case { raw: "", text: "", scope: "", empty: true, filters: &[], arg_file: "", extras_file: &[], routes_file: true, routes_apps: true },
            Case { raw: "   ", text: "", scope: "", empty: true, filters: &[], arg_file: "", extras_file: &[], routes_file: true, routes_apps: true },
            Case { raw: "firefox", text: "firefox", scope: "", empty: false, filters: &[], arg_file: "firefox", extras_file: &[], routes_file: true, routes_apps: true },
            Case { raw: "file:report.pdf", text: "", scope: "file", empty: false, filters: &[("file", "report.pdf")], arg_file: "report.pdf", extras_file: &[], routes_file: true, routes_apps: false },
            Case { raw: "file:", text: "", scope: "file", empty: false, filters: &[("file", "")], arg_file: "", extras_file: &[], routes_file: true, routes_apps: false },
            Case { raw: "file:report budget", text: "budget", scope: "file", empty: false, filters: &[("file", "report")], arg_file: "report budget", extras_file: &[], routes_file: true, routes_apps: false },
            Case { raw: "music:\"kind of blue\" jazz", text: "jazz", scope: "music", empty: false, filters: &[("music", "kind of blue")], arg_file: "jazz", extras_file: &[("music", "kind of blue")], routes_file: false, routes_apps: false },
            Case { raw: "music:'single quoted' tail", text: "tail", scope: "music", empty: false, filters: &[("music", "single quoted")], arg_file: "tail", extras_file: &[("music", "single quoted")], routes_file: false, routes_apps: false },
            Case { raw: "=2+2", text: "", scope: "calc", empty: false, filters: &[("calc", "2+2")], arg_file: "", extras_file: &[("calc", "2+2")], routes_file: false, routes_apps: false },
            Case { raw: ">lock", text: "", scope: "run", empty: false, filters: &[("run", "lock")], arg_file: "", extras_file: &[("run", "lock")], routes_file: false, routes_apps: false },
            Case { raw: "?how to", text: "", scope: "web", empty: false, filters: &[("web", "how to")], arg_file: "", extras_file: &[("web", "how to")], routes_file: false, routes_apps: false },
            Case { raw: "/etc", text: "", scope: "command", empty: false, filters: &[("command", "etc")], arg_file: "", extras_file: &[("command", "etc")], routes_file: false, routes_apps: false },
            Case { raw: "/etc/passwd", text: "", scope: "file", empty: false, filters: &[("file", "etc/passwd")], arg_file: "etc/passwd", extras_file: &[], routes_file: true, routes_apps: false },
            Case { raw: "/ls", text: "", scope: "command", empty: false, filters: &[("command", "ls")], arg_file: "", extras_file: &[("command", "ls")], routes_file: false, routes_apps: false },
            Case { raw: "/path/with/slash", text: "", scope: "file", empty: false, filters: &[("file", "path/with/slash")], arg_file: "path/with/slash", extras_file: &[], routes_file: true, routes_apps: false },
            Case { raw: "~/notes", text: "~/notes", scope: "", empty: false, filters: &[], arg_file: "~/notes", extras_file: &[], routes_file: true, routes_apps: true },
            Case { raw: "win:", text: "", scope: "win", empty: false, filters: &[("win", "")], arg_file: "", extras_file: &[("win", "")], routes_file: false, routes_apps: false },
            Case { raw: "anything:x", text: "anything:x", scope: "", empty: false, filters: &[], arg_file: "anything:x", extras_file: &[], routes_file: true, routes_apps: true },
            Case { raw: "https://example.com", text: "https://example.com", scope: "", empty: false, filters: &[], arg_file: "https://example.com", extras_file: &[], routes_file: true, routes_apps: true },
            Case { raw: "git:omarchy commit", text: "commit", scope: "git", empty: false, filters: &[("git", "omarchy")], arg_file: "commit", extras_file: &[("git", "omarchy")], routes_file: false, routes_apps: false },
            Case { raw: "FILE:x", text: "", scope: "file", empty: false, filters: &[("file", "x")], arg_file: "x", extras_file: &[], routes_file: true, routes_apps: false },
            Case { raw: "File:x", text: "", scope: "file", empty: false, filters: &[("file", "x")], arg_file: "x", extras_file: &[], routes_file: true, routes_apps: false },
            Case { raw: "file:report format:pdf", text: "", scope: "file", empty: false, filters: &[("file", "report"), ("format", "pdf")], arg_file: "report", extras_file: &[("format", "pdf")], routes_file: true, routes_apps: false },
            Case { raw: "file:a file:b", text: "", scope: "file", empty: false, filters: &[("file", "b")], arg_file: "b", extras_file: &[], routes_file: true, routes_apps: false },
            Case { raw: "a:b:c", text: "a:b:c", scope: "", empty: false, filters: &[], arg_file: "a:b:c", extras_file: &[], routes_file: true, routes_apps: true },
            Case { raw: "  spaced   out  ", text: "spaced out", scope: "", empty: false, filters: &[], arg_file: "spaced out", extras_file: &[], routes_file: true, routes_apps: true },
            Case { raw: "file:report format:pdf git:omarchy", text: "", scope: "file", empty: false, filters: &[("file", "report"), ("format", "pdf"), ("git", "omarchy")], arg_file: "report", extras_file: &[("format", "pdf"), ("git", "omarchy")], routes_file: true, routes_apps: false },
            Case { raw: "tz:tokyo 9am", text: "9am", scope: "tz", empty: false, filters: &[("tz", "tokyo")], arg_file: "9am", extras_file: &[("tz", "tokyo")], routes_file: false, routes_apps: false },
            Case { raw: "unknown:value known:x", text: "unknown:value known:x", scope: "", empty: false, filters: &[], arg_file: "unknown:value known:x", extras_file: &[], routes_file: true, routes_apps: true },
            Case { raw: "file:\"quoted value\"", text: "", scope: "file", empty: false, filters: &[("file", "quoted value")], arg_file: "quoted value", extras_file: &[], routes_file: true, routes_apps: false },
            Case { raw: "file:unterminated \"quote", text: "\"quote", scope: "file", empty: false, filters: &[("file", "unterminated")], arg_file: "unterminated \"quote", extras_file: &[], routes_file: true, routes_apps: false },
            Case { raw: "in:~/work report", text: "report", scope: "in", empty: false, filters: &[("in", "~/work")], arg_file: "report", extras_file: &[("in", "~/work")], routes_file: false, routes_apps: false },
            Case { raw: "sp:kind of blue type:album", text: "of blue", scope: "sp", empty: false, filters: &[("sp", "kind"), ("type", "album")], arg_file: "of blue", extras_file: &[("sp", "kind"), ("type", "album")], routes_file: false, routes_apps: false },
        ];

        for c in cases {
            let q = Query::parse(c.raw, 1, Some(&known));
            assert_eq!(q.text, c.text, "text of {:?}", c.raw);
            assert_eq!(q.scope, c.scope, "scope of {:?}", c.raw);
            assert_eq!(q.empty, c.empty, "empty of {:?}", c.raw);
            let got: Vec<(&str, &str)> = q
                .filters
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            assert_eq!(got, c.filters, "filters of {:?}", c.raw);
            assert_eq!(
                q.arg_for("file", &aliases_file),
                c.arg_file,
                "arg_for(file) of {:?}",
                c.raw
            );
            let extras_map = q.extras("file", &aliases_file);
            let extras: Vec<(&str, &str)> = extras_map
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            assert_eq!(extras, c.extras_file, "extras(file) of {:?}", c.raw);
            assert_eq!(
                q.routes_to("file", &aliases_file),
                c.routes_file,
                "routes_to(file) of {:?}",
                c.raw
            );
            assert_eq!(
                q.routes_to("apps", &aliases_apps),
                c.routes_apps,
                "routes_to(apps) of {:?}",
                c.raw
            );
        }
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
