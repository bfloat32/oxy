//! `ch:` — clipboard history, read from the JSON Omarchy's own clipboard
//! overlay already keeps. A port of `bin/oxy-clipboard-history`.
//!
//!   ch:            everything, newest first
//!   ch:token       only entries containing that
//!
//! Reads the same file the overlay writes rather than keeping a second
//! history, so both views always agree and nothing has to be migrated.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::shellquote::quote;

#[derive(Default)]
pub struct Clip {
    cache: Arc<Mutex<Option<ClipCache>>>,
}

/// The parsed history entries, stamped with the file's mtime — the overlay
/// rewrites the file on every copy, so a match is safe to trust.
struct ClipCache {
    stamp: Option<std::time::SystemTime>,
    entries: Arc<Vec<Value>>,
}

// Three caps, because one entry can be enormous and this runs on every
// keystroke. The overlay that writes the file stores whatever was copied, and
// a single 1.2MB paste made `ch:the` cost 537ms of jq per keystroke: 33 times
// the whole rest of the answer. None of these caps change which entries
// exist, only how much of one entry is read, drawn, or carried on a command
// line.
/// How far into an entry a query is matched. Searching a copied file is
/// looking for something you recognise near the start of it, not word 200,000.
const SCAN: usize = 32768;
/// How much of an entry reaches the preview pane. Every row's preview is held
/// in memory by the launcher for as long as the rows are on screen, and nobody
/// reads a megabyte in a side panel.
const PREVIEW: usize = 4000;
/// The largest entry still pasted by putting it on a command line. Anything
/// bigger is re-read from the file by index instead: ARG_MAX is about 2MB and
/// a `printf %s <1.2MB> | wl-copy` is a failure waiting for a slightly longer
/// paste.
const INLINE: usize = 65536;

/// The first `n` characters — jq's `[0:n]` slices are on characters, and a
/// copied page of unicode is still one entry.
fn head(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn history_path() -> std::path::PathBuf {
    crate::dirs::state_home().join("omarchy/clipboard-history.json")
}

fn query_blocking(cache: Arc<Mutex<Option<ClipCache>>>, arg: &str) -> NativeOutcome {
    let hist = history_path();
    let mtime = std::fs::metadata(&hist).and_then(|m| m.modified()).ok();
    let entries = {
        let mut c = cache.lock().unwrap();
        match c.as_ref() {
            Some(c) if c.stamp == mtime && mtime.is_some() => c.entries.clone(),
            _ => {
                let parsed: Vec<Value> = std::fs::read_to_string(&hist)
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or_default();
                let entries = Arc::new(parsed);
                *c = Some(ClipCache {
                    stamp: mtime,
                    entries: entries.clone(),
                });
                entries
            }
        }
    };

    let q = arg.trim().to_lowercase();
    let mut rows = Vec::new();
    for (i, v) in entries.iter().enumerate() {
        let text = v.get("text").and_then(Value::as_str).unwrap_or("");
        let path = v.get("path").and_then(Value::as_str).unwrap_or("");
        if text.is_empty() && path.is_empty() {
            continue;
        }
        if !q.is_empty() && !head(text, SCAN).to_lowercase().contains(&q) {
            continue;
        }
        let mime = v.get("mime").and_then(Value::as_str).unwrap_or("");
        let is_image = mime.starts_with("image/");
        let short = head(text, PREVIEW);
        let len = text.chars().count();

        let id = if path.is_empty() {
            format!("{}{i}", head(text, 64))
        } else {
            path.to_string()
        };
        let title = if is_image {
            format!("Image  ·  {}", path.rsplit('/').next().unwrap_or(""))
        } else {
            head(&short.split_whitespace().collect::<Vec<_>>().join(" "), 70).to_string()
        };
        let preview = if is_image {
            String::new()
        } else if len > PREVIEW {
            format!(
                "{short}\n\n… {} more characters. Enter copies all of it.",
                len - PREVIEW
            )
        } else {
            text.to_string()
        };
        let exec = if is_image {
            format!(
                "omarchy-clipboard-paste-file --copy-only {} {}",
                quote(mime),
                quote(path)
            )
        } else if len > INLINE {
            // Read back out of the file rather than carried here. The whole
            // entry still reaches the clipboard; it just never travels as an
            // argument.
            format!(
                "jq -j --argjson i {i} {} {} | wl-copy",
                quote(".[$i].text // \"\""),
                quote(&hist.to_string_lossy())
            )
        } else {
            format!("printf %s {} | wl-copy", quote(text))
        };
        rows.push(json!({
            "id": id,
            "title": title,
            "subtitle": "",
            "preview": preview,
            "art": if is_image { crate::native::file::file_url(path) } else { String::new() },
            "exec": exec,
            "score": 95000 - (i as i64) * 100,
        }));
    }
    NativeOutcome::Rows(rows)
}

impl NativeExt for Clip {
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
                .unwrap_or(NativeOutcome::Empty)
        })
    }
}
