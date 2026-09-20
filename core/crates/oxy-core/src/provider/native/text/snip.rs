//! `snip` — the snippet library: `~/.config/omarchy/oxy-snippets.json`
//! parsed in-process, one row per entry with its line/character counts.
//! Enter copies, Ctrl+K types — both still the clipboard daemon's work.
//!
//! The file is a flat object of name to text, and the row keeps the text
//! whole — `text`/`preview` — because the name is a label on it, not the
//! subject: you named it, so you know what it is called and not what is in
//! it. No file is silence, the same silence the `when` keeps it under.

use std::future::Future;
use std::pin::Pin;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

#[derive(Default)]
pub struct Snip;

/// `${XDG_CONFIG_HOME:-$HOME/.config}/omarchy/oxy-snippets.json`.
fn snippets_path() -> std::path::PathBuf {
    crate::settings::paths::config_home().join("omarchy/oxy-snippets.json")
}

/// jq's `gsub("\\s+"; " ") | .[0:110]`: every whitespace run collapses to a
/// single space — a leading run included — then the first 110 characters.
/// Newlines in a one-line row read as missing text, so the subtitle shows
/// the shape of the snippet rather than only its first line.
fn one_line(text: &str) -> String {
    let mut out = String::new();
    let mut in_ws = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out.chars().take(110).collect()
}

/// The jq program, kept as a pure function: the file's text in, the rows
/// out. jq's `to_entries` walks `keys_unsorted` — insertion order — which
/// is the order serde_json's preserve_order map keeps as well, so rows
/// come out in file order and the score marks it.
fn rows(text: &str, query: &str) -> Vec<Value> {
    // jq failing or a file that is not an object of strings is silence —
    // a file mid-edit is not an error row.
    let Ok(file) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let Some(obj) = file.as_object() else {
        return Vec::new();
    };
    let q = query.to_lowercase();
    let mut out = Vec::new();
    let mut i = 0i64;
    for (name, value) in obj {
        // Anything that is not a non-empty string is skipped rather than
        // turned into a broken row.
        let Some(value) = value.as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        // `.key | ascii_downcase` — the haystacks fold ASCII only, like jq.
        if !q.is_empty()
            && !name.to_ascii_lowercase().contains(&q)
            && !value.to_ascii_lowercase().contains(&q)
        {
            continue;
        }
        let copy = format!("printf %s {} | wl-copy", quote(value));
        out.push(json!({
            "id": name,
            "title": name,
            "subtitle": one_line(value),
            "preview": value,
            "text": value,
            "lines": value.split('\n').count(),
            "chars": value.chars().count(),
            "exec": copy,
            "score": 90000 - i * 100,
            "actions": [
                { "title": "Copy Snippet", "shortcut": "↵", "exec": copy },
                // The launcher has to be gone before the keystrokes are
                // sent, or the window that gets typed into is the search box.
                { "title": "Type Into Window",
                  "exec": format!("sleep 0.2; wtype {}", quote(value)) },
                { "title": "Copy Name",
                  "exec": format!("printf %s {} | wl-copy", quote(name)) },
            ],
        }));
        i += 1;
    }
    out
}

impl NativeExt for Snip {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let arg = ctx.arg.clone();
        Box::pin(async move {
            let path = snippets_path();
            // The manifest's `when` is `test -f` on this file. Declining to
            // the script keeps that silence rather than drawing empty rows —
            // the script, run in its place, says the same nothing.
            if !path.is_file() {
                return NativeOutcome::Fallback;
            }
            let text = tokio::fs::read_to_string(&path).await.unwrap_or_default();
            NativeOutcome::Rows(rows(&text, &arg))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// File order, not sorted order: `sig` is declared first and stays
    /// first, and the score counts the kept entries — `num` and `empty`
    /// never reach it.
    const FIXTURE: &str = r#"{
        "sig": "Ada Lovelace\nada@example.com",
        "addr": "12  Example   Road",
        "num": 42,
        "empty": "",
        "greet": "hi"
    }"#;

    #[test]
    fn every_non_empty_string_is_a_row_in_file_order() {
        let rows = rows(FIXTURE, "");
        let ids: Vec<&str> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["sig", "addr", "greet"]);
        assert_eq!(rows[0]["score"], 90000);
        assert_eq!(rows[1]["score"], 89900);
        assert_eq!(rows[2]["score"], 89800);
    }

    #[test]
    fn the_row_is_the_jq_object() {
        let row = rows(FIXTURE, "")[0].clone();
        let copy = "printf %s 'Ada Lovelace\nada@example.com' | wl-copy";
        assert_eq!(row["id"], "sig");
        assert_eq!(row["title"], "sig");
        // The subtitle is the snippet's shape on one line.
        assert_eq!(row["subtitle"], "Ada Lovelace ada@example.com");
        assert_eq!(row["preview"], "Ada Lovelace\nada@example.com");
        assert_eq!(row["text"], "Ada Lovelace\nada@example.com");
        assert_eq!(row["lines"], 2);
        assert_eq!(row["chars"], 28);
        assert_eq!(row["exec"], copy);
        assert_eq!(
            row["actions"],
            json!([
                { "title": "Copy Snippet", "shortcut": "↵", "exec": copy },
                { "title": "Type Into Window",
                  "exec": "sleep 0.2; wtype 'Ada Lovelace\nada@example.com'" },
                { "title": "Copy Name", "exec": "printf %s 'sig' | wl-copy" },
            ])
        );
    }

    #[test]
    fn the_query_matches_name_or_body_ignoring_ascii_case() {
        // A query hits the name…
        assert_eq!(rows(FIXTURE, "SIG").len(), 1);
        // …or the text…
        let hit = rows(FIXTURE, "example");
        assert_eq!(hit.len(), 2);
        assert_eq!(hit[0]["id"], "sig"); // filtered, file order kept
        // …and a miss is an empty answer, not an error row.
        assert!(rows(FIXTURE, "zzz-no-snippet").is_empty());
    }

    #[test]
    fn a_file_that_is_not_an_object_of_strings_is_silence() {
        assert!(rows("not json", "").is_empty());
        assert!(rows("[1,2,3]", "").is_empty());
        assert!(rows("\"just a string\"", "").is_empty());
        assert!(rows("{}", "").is_empty());
        assert!(rows("", "").is_empty());
    }

    #[test]
    fn subtitle_collapses_whitespace_and_truncates() {
        assert_eq!(one_line("a\nb  c\td"), "a b c d");
        // A leading run folds to one space — gsub replaces, it does not trim.
        assert_eq!(one_line(" \n a"), " a");
        assert_eq!(one_line(""), "");
        let long = "x".repeat(200);
        assert_eq!(one_line(&long).chars().count(), 110);
    }
}
