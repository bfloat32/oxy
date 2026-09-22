//! The row half — the jq program at the end of `bin/oxy-define`, plus the
//! two standalone rows it prints when the dictionary cannot help. jq's half
//! of the contract is reproduced rather than approximated: where the script
//! relies on `//` for absent fields, the same null/false/absent readings
//! apply, and where jq would die on a malformed shape the functions return
//! `None`, which is the script printing nothing.

use serde_json::{Value, json};

use crate::support::quote::quote;

/// `.field` — jq indexes objects and glides over `null`; a number or a
/// string there is the malformed shape the whole jq run dies on, so the
/// outer Option is that death.
fn field<'a>(v: &'a Value, key: &str) -> Option<Option<&'a Value>> {
    match v {
        Value::Object(o) => Some(o.get(key)),
        Value::Null => Some(None),
        _ => None,
    }
}

/// `x // ""` where a string is required downstream: null/false/absent read
/// as "", a string is itself, anything else is the jq-dies case.
fn jstr(v: Option<Option<&Value>>) -> Option<String> {
    match v? {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Some(String::new()),
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => None,
    }
}

/// `x // []`: null/false/absent iterate as nothing, an array as itself,
/// anything else is the jq-dies case.
fn jarr(v: Option<Option<&Value>>) -> Option<Vec<&Value>> {
    match v? {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Some(Vec::new()),
        Some(Value::Array(a)) => Some(a.iter().collect()),
        Some(_) => None,
    }
}

/// `map(select(. != null and . != "")) | unique | join(", ")` — the API
/// repeats a meaning-level synonym list on every definition under it, so
/// the two are merged and deduplicated rather than shown twice; jq's
/// `unique` sorts, which is why the list comes out alphabetical.
fn joinlist(a: Option<Vec<&Value>>, b: Option<Vec<&Value>>) -> Option<String> {
    let mut v: Vec<String> = Vec::new();
    for item in a?.into_iter().chain(b?) {
        match item {
            Value::Null => {}
            Value::String(s) if s.is_empty() => {}
            Value::String(s) => v.push(s.clone()),
            // A kept non-string is what `join` chokes on.
            _ => return None,
        }
    }
    v.sort();
    v.dedup();
    Some(v.join(", "))
}

/// One sense pulled flat: the word, its pronunciation, part of speech, the
/// 1-based index inside its meaning, and the text/example/synonyms/antonyms
/// the row is built from.
struct Sense {
    word: String,
    ipa: String,
    pos: String,
    sense: usize,
    text: String,
    example: String,
    syn: String,
    ant: String,
}

/// The flattening half of the jq program: every entry's every meaning's
/// every definition, in order, keeping the ones with text. `None` is a
/// shape jq could not print — the script emits nothing then, so we do too.
fn senses(body: &Value) -> Option<Vec<Sense>> {
    let mut out = Vec::new();
    for entry in body.as_array()? {
        let word = jstr(field(entry, "word"))?;
        let phonetic = jstr(field(entry, "phonetic"))?;
        // `phonetic`, else the first `phonetics[].text` that is not "".
        let ipa = if !phonetic.is_empty() {
            phonetic
        } else {
            let mut found = String::new();
            for p in jarr(field(entry, "phonetics"))? {
                let t = jstr(field(p, "text"))?;
                if !t.is_empty() {
                    found = t;
                    break;
                }
            }
            found
        };
        for meaning in jarr(field(entry, "meanings"))? {
            let pos = jstr(field(meaning, "partOfSpeech"))?;
            for (i, d) in jarr(field(meaning, "definitions"))?.into_iter().enumerate() {
                let text = jstr(field(d, "definition"))?;
                let example = jstr(field(d, "example"))?;
                let syn = joinlist(jarr(field(d, "synonyms")), jarr(field(meaning, "synonyms")))?;
                let ant = joinlist(jarr(field(d, "antonyms")), jarr(field(meaning, "antonyms")))?;
                if text.is_empty() {
                    continue;
                }
                out.push(Sense {
                    word: word.clone(),
                    ipa: ipa.clone(),
                    pos: pos.clone(),
                    sense: i + 1,
                    text,
                    example,
                    syn,
                    ant,
                });
            }
        }
    }
    Some(out)
}

/// One sense → the jq row object, field for field. The strip beside the
/// pane (detail) is the only place a row can say it is answering a spelling
/// other than the one that was typed.
fn sense_row(i: usize, s: &Sense, note: &str) -> Value {
    let title = if s.sense > 1 {
        format!("{}  ·  {}  {}", s.word, s.pos, s.sense)
    } else {
        format!("{}  ·  {}", s.word, s.pos)
    };
    // `[ .word, (ipa if it is not ""), .pos ] | join("   ")` — three spaces.
    let mut head = vec![s.word.clone()];
    if !s.ipa.is_empty() {
        head.push(s.ipa.clone());
    }
    head.push(s.pos.clone());
    let head = head.join("   ");

    let mut parts: Vec<String> = Vec::new();
    if !note.is_empty() {
        parts.push(format!("{note}\n"));
    }
    parts.push(head);
    parts.push(String::new());
    parts.push(s.text.clone());
    if !s.example.is_empty() {
        parts.push(format!("\n“{}”", s.example));
    }
    if !s.syn.is_empty() {
        parts.push(format!("\nSynonyms: {}", s.syn));
    }
    if !s.ant.is_empty() {
        parts.push(format!("Antonyms: {}", s.ant));
    }
    let preview = parts.join("\n");

    let copy = format!("printf %s {} | wl-copy", quote(&s.text));
    let detail = if !note.is_empty() {
        note.to_string()
    } else if !s.syn.is_empty() {
        format!("syn. {}", s.syn)
    } else {
        s.pos.clone()
    };
    let mut actions = vec![
        json!({ "title": "Copy Definition", "shortcut": "↵", "exec": copy }),
        json!({ "title": "Copy Word",
                "exec": format!("printf %s {} | wl-copy", quote(&s.word)) }),
        json!({ "title": "Copy Sense",
                "exec": format!("printf %s {} | wl-copy",
                    quote(&format!("{} ({}): {}", s.word, s.pos, s.text))) }),
    ];
    if !s.syn.is_empty() {
        actions.push(json!({ "title": "Copy Synonyms",
                             "exec": format!("printf %s {} | wl-copy", quote(&s.syn)) }));
    }

    json!({
        "id": format!("def-{}-{}", s.pos, i),
        "title": title,
        "subtitle": s.text,
        "detail": detail,
        "preview": preview,
        "exec": copy,
        "score": 95000 - i as i64 * 100,
        "view": "split",
        "glyph": "",
        "actions": actions,
    })
}

/// The jq program end to end: one row per sense, then a single thesaurus
/// row at the end gathering every synonym the entry carries — last because
/// a definition is what `def:` was typed for.
pub(super) fn define_rows(body: &Value, note: &str) -> Option<Vec<Value>> {
    let senses = senses(body)?;
    // `[ $rows[].syn | select(. != "") ] | join(", ") | split(", ") |
    //  unique | join(", ")` — every synonym across every row.
    let mut all: Vec<String> = Vec::new();
    for s in &senses {
        if !s.syn.is_empty() {
            all.extend(s.syn.split(", ").map(str::to_string));
        }
    }
    all.sort();
    all.dedup();
    let all_syn = all.join(", ");

    let mut out: Vec<Value> = senses
        .iter()
        .enumerate()
        .map(|(i, s)| sense_row(i, s, note))
        .collect();
    if !all_syn.is_empty() {
        let word = &senses[0].word;
        let exec = format!("printf %s {} | wl-copy", quote(&all_syn));
        out.push(json!({
            "id": "def-synonyms",
            "title": format!("{word}  ·  synonyms"),
            "subtitle": all_syn,
            "detail": "Thesaurus",
            "preview": format!("Synonyms for {word}\n\n{}", all_syn.replace(", ", "\n")),
            "exec": exec,
            "score": 80000,
            "view": "split",
            "glyph": "",
            "actions": [{ "title": "Copy Synonyms", "shortcut": "↵", "exec": exec }],
        }));
    }
    Some(out)
}

/// The dictionary could not be reached at all. Silence here reads as a
/// broken keyword, and it is the one thing a person can act on: they know
/// why.
pub(super) fn offline_row(term: &str) -> Value {
    json!({
        "id": "def-offline",
        "title": "Dictionary unreachable",
        "subtitle": format!("Could not look up “{term}”"),
        "detail": "No network, or api.dictionaryapi.dev is down",
        "preview": format!("Could not reach api.dictionaryapi.dev to look up “{term}”.\n\nWords looked up before are still answered from the cache."),
        "score": 90000,
        "view": "split",
        "glyph": "",
        "actions": [],
    })
}

/// No entry under any spelling. Worth answering out loud rather than going
/// quiet: the dictionary was reached and it said no, which is a different
/// thing from the keyword not working.
pub(super) fn no_def_row(term: &str, lower: &str) -> Value {
    let exec = format!(
        "xdg-open {}",
        crate::provider::native::util::shq(&format!(
            "https://www.google.com/search?q=define+{}",
            lower.replace(' ', "+")
        ))
    );
    json!({
        "id": "def-none",
        "title": format!("No definition for \"{term}\""),
        "subtitle": "Dictionary",
        "detail": "Enter searches the web instead",
        "preview": format!("No dictionary entry for \"{term}\".\n\nEnter searches the web for it."),
        "exec": exec,
        "score": 90000,
        "view": "split",
        "glyph": "",
        "actions": [{ "title": "Search the Web", "shortcut": "↵", "exec": exec }],
    })
}
