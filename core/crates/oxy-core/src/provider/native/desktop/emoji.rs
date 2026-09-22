//! `emoji:` — find an emoji by name and copy the character. A port of
//! `bin/oxy-emoji`: the data is the list Omarchy's emoji picker already
//! ships, read in place, and the ranking is the same banded matcher —
//! a whole-word hit beats a prefix hit beats a substring hit, and every
//! typed word has to land somewhere.
//!
//! What you used lately leads the empty query. The script recorded it with
//! `oxy-emoji --used` inside the exec; the native rows carry a `remember`
//! extra instead and the engine writes the same file, so the record path
//! works with no script on PATH at all.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use super::emoji_data::{DEFAULTS, intents};
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

/// The parsed data file, behind a lock so the worker can run this provider
/// on `spawn_blocking` — reading and ranking ~1900 entries is synchronous
/// work, and it has no business on a runtime worker thread.
///
/// Keyed on the file's mtime: the picker ships this list with the shell, so
/// it changes when the system updates, not between keystrokes.
#[derive(Default)]
pub struct Emoji {
    cache: Arc<Mutex<Option<Cached>>>,
}

struct Cached {
    mtime: Option<std::time::SystemTime>,
    entries: Arc<Vec<Entry>>,
}

/// Read and parse the data file — the only caller that pays for it is a
/// cache miss.
fn load(path: &Path) -> Option<Arc<Vec<Entry>>> {
    let text = std::fs::read_to_string(path).ok()?;
    let data = serde_json::from_str::<Vec<Value>>(&text).ok()?;
    let all: Vec<Entry> = data
        .iter()
        .filter_map(|v| {
            let e = v.get("e")?.as_str()?.to_string();
            let k = v.get("k")?.as_str()?.to_lowercase().replace('_', " ");
            Some(Entry {
                e,
                w: k.split(' ')
                    .filter(|w| !w.is_empty())
                    .map(String::from)
                    .collect(),
                k,
            })
        })
        .collect();
    Some(Arc::new(all))
}

/// The file both pickers read. `OXY_EMOJI_DATA` exists so a dev checkout —
/// or a test — can point at a copy without owning `/usr/share/omarchy`.
fn data_path() -> PathBuf {
    std::env::var("OXY_EMOJI_DATA")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/share/omarchy/shell/plugins/emojis/emojis.json"))
}

const KEEP_RECENT: usize = 24;

/// One data entry, prepared once: the character, its keyword string with
/// underscores already read as spaces, and the words of it.
struct Entry {
    e: String,
    k: String,
    w: Vec<String>,
}

/// The jq `rank1`: whole word beats prefix beats substring; inside a band an
/// earlier and shorter keyword string wins, which puts ⭐ "star" above
/// 💫 "dizzy star".
fn rank1(e: &Entry, needle: &str) -> i64 {
    let band = if e.w.iter().any(|w| w == needle) {
        3000
    } else if e.w.iter().any(|w| w.starts_with(needle)) {
        2000
    } else if e.k.contains(needle) {
        1000
    } else {
        return 0;
    };
    let at = e.k.find(needle).map(|i| i as i64).unwrap_or(200).min(200);
    let len = (e.k.len() as i64).min(100);
    band + (200 - at) + (100 - len)
}

/// Every word has to land somewhere, not the whole phrase in one piece —
/// "crying laughing" is nowhere in 🤣's keywords but both words are. A phrase
/// that appears whole is still worth more than the same words scattered.
fn rank(e: &Entry, tokens: &[String], needle: &str) -> i64 {
    if tokens.len() == 1 {
        return rank1(e, &tokens[0]);
    }
    let mut sum = 0i64;
    for t in tokens {
        let r = rank1(e, t);
        if r == 0 {
            return 0;
        }
        sum += r;
    }
    sum + if e.k.contains(needle) { 1500 } else { 0 }
}

/// An entry that ranks at all contains every word, so it contains the first
/// one — filtering on it alone is the difference between a keystroke and a
/// wait.
fn hits<'a>(all: &'a [Entry], tokens: &[String]) -> Vec<&'a Entry> {
    all.iter()
        .filter(|e| e.k.contains(tokens[0].as_str()))
        .collect()
}

/// `max_by` keeps the first of equals — order inside a rank is the data's.
fn best<'a>(all: &'a [Entry], needle: &str) -> Option<&'a Entry> {
    let tokens: Vec<String> = needle
        .split(' ')
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect();
    if tokens.is_empty() {
        return None;
    }
    let mut found: Option<(&Entry, i64)> = None;
    for e in hits(all, &tokens) {
        let r = rank(e, &tokens, needle);
        if r <= 0 {
            continue;
        }
        if found.map(|(_, fr)| r > fr).unwrap_or(true) {
            found = Some((e, r));
        }
    }
    found.map(|(e, _)| e)
}

/// The keyword string is a name followed by aliases with words repeated
/// between them; dropping repeats leaves the label. Only shown rows get one.
fn label(e: &Entry) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for w in &e.w {
        if !seen.contains(&w.as_str()) {
            seen.push(w);
        }
    }
    let name = seen.join(" ");
    name.chars().take(64).collect()
}

fn mru_path() -> PathBuf {
    crate::settings::paths::state_home().join("omarchy/oxy-emoji-recent")
}

fn recent_chars() -> Vec<String> {
    std::fs::read_to_string(mru_path())
        .ok()
        .map(|t| {
            t.lines()
                .map(|l| l.to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn row(e: &Entry, mru: bool, score: i64) -> Value {
    let name = label(e);
    let copy = format!("printf %s {} | wl-copy", quote(&e.e));
    // Recording runs before the copy rather than after it: the copy is the
    // thing Enter was pressed for, and a slow write must not sit between the
    // keypress and the clipboard. The engine honors `remember` before exec.
    let remember = json!({"file": "emoji-recent", "value": e.e, "keep": KEEP_RECENT});
    json!({
        "id": e.e,
        "title": name,
        "glyph": e.e,
        "copyText": e.e,
        // The strip beside the grid is the only place a row can say it is
        // here because you used it.
        "subtitle": if mru { "Recently used" } else { "Emoji" },
        "exec": copy,
        "score": score,
        "actions": [
            { "title": "Copy Emoji", "shortcut": "↵", "exec": copy,
              "remember": remember },
            // The launcher closes first, so the keystrokes need a window to
            // land in by the time they are sent.
            { "title": "Type It", "exec": format!("sleep 0.2; wtype {}", quote(&e.e)),
              "remember": remember },
            { "title": "Copy Name", "exec": format!("printf %s {} | wl-copy", quote(&name)) },
        ]
    })
}

impl NativeExt for Emoji {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let cache = self.cache.clone();
        let arg = ctx.arg.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || query_blocking(cache, &arg))
                .await
                .unwrap_or(NativeOutcome::Fallback)
        })
    }
}

/// The whole question, off the runtime threads.
fn query_blocking(cache: Arc<Mutex<Option<Cached>>>, arg: &str) -> NativeOutcome {
    let path = data_path();
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let all = {
        let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
        match c.as_ref() {
            // An mtime we could not read is never trusted as a cache key.
            Some(c) if c.mtime == mtime && mtime.is_some() => c.entries.clone(),
            _ => match load(&path) {
                Some(entries) => {
                    *c = Some(Cached {
                        mtime,
                        entries: entries.clone(),
                    });
                    entries
                }
                // Without the data file the script leg cannot run either —
                // `when` already says so — so this is empty, not a fallback.
                None => return NativeOutcome::Empty,
            },
        }
    };

    // Trimmed and collapsed: "  thumbs   up " is one query.
    let q = arg
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    let mut picked: Vec<(&Entry, bool)> = Vec::new(); // (entry, mru)
    if q.is_empty() {
        // What you reached for lately, then the standing set — a
        // character in both is kept once, at the position it earned.
        for ch in recent_chars() {
            if let Some(e) = all.iter().find(|e| e.e == ch) {
                picked.push((e, true));
            }
        }
        for name in DEFAULTS {
            if let Some(e) = best(&all, name) {
                picked.push((e, false));
            }
        }
    } else {
        let tokens: Vec<String> = q
            .split(' ')
            .filter(|t| !t.is_empty())
            .map(String::from)
            .collect();
        // What the words mean first, then what they name. The intent
        // list is short, so a feeling leads without the ordinary
        // hits being thrown away.
        // `[$q, $q|sub(...)] | unique` — jq's unique sorts, so the
        // intent lookups run in lexicographic order, not query-then-
        // stripped. One suffix only ever matches the end.
        let mut keys = vec![q.clone()];
        for suffix in [" face", " emoji", " emojis", " sign", " symbol", " icon"] {
            if let Some(stripped) = q.strip_suffix(suffix) {
                keys.push(stripped.to_string());
                break;
            }
        }
        keys.sort();
        keys.dedup();
        for key in &keys {
            for name in intents(key) {
                if let Some(e) = best(&all, name) {
                    picked.push((e, false));
                }
            }
        }
        let mut ranked: Vec<(&Entry, i64)> = hits(&all, &tokens)
            .into_iter()
            .filter_map(|e| {
                let r = rank(e, &tokens, &q);
                (r > 0).then_some((e, r))
            })
            .collect();
        ranked.sort_by_key(|a| std::cmp::Reverse(a.1));
        picked.extend(ranked.into_iter().map(|(e, _)| (e, false)));

        if picked.is_empty() {
            // Nothing matched every word: the words that did match
            // are added up, which is what turns "crying laughing
            // face" from silence into 😂 and 🤣.
            let mut loose: Vec<(&Entry, i64)> = all
                .iter()
                .filter_map(|e| {
                    let r: i64 = tokens.iter().map(|t| rank1(e, t)).sum();
                    (r > 0).then_some((e, r))
                })
                .collect();
            loose.sort_by_key(|a| std::cmp::Reverse(a.1));
            picked.extend(loose.into_iter().map(|(e, _)| (e, false)));
        }
    }

    // Dedupe in order — sorting would throw away the ordering that
    // is the whole point — and cap at a screen of grid.
    let mut seen: Vec<&str> = Vec::new();
    let mut rows = Vec::new();
    for (e, mru) in picked {
        if seen.contains(&e.e.as_str()) {
            continue;
        }
        seen.push(&e.e);
        rows.push(row(e, mru, 90000 - rows.len() as i64 * 100));
        if rows.len() >= 60 {
            break;
        }
    }
    NativeOutcome::Rows(rows)
}
