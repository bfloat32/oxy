//! `calchist` — the record of what the calculator has already answered, for
//! the `calc:` keyword. A port of the read leg of `bin/oxy-calc-history`.
//!
//!   calc:            every answer you have kept, newest first
//!   calc:usd         the ones whose expression or answer matches
//!   calc:2+2         the built-in calculator answers on top; the history below
//!                    shows the last time you asked
//!
//! The write leg stays a script: `oxy-calc-history record <expr> <answer>` is
//! what the calculator's accept action execs, and an exec is still the right
//! shape for a locked file append.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

#[derive(Default)]
pub struct CalcHist {
    cache: Arc<Mutex<Option<HistCache>>>,
}

/// The parsed history, stamped with the file's mtime — every `record` rewrites
/// the file, so a match is safe to trust.
struct HistCache {
    stamp: Option<std::time::SystemTime>,
    entries: Arc<Vec<Value>>,
}

fn history_path() -> std::path::PathBuf {
    crate::settings::paths::state_home().join("omarchy/oxy-calc-history.json")
}

/// The accessory is the age, terse: "now", "4m", "9h", "3d", "2mo".
fn when(age: i64) -> String {
    if age < 60 {
        "now".to_string()
    } else if age < 3600 {
        format!("{}m", age / 60)
    } else if age < 86400 {
        format!("{}h", age / 3600)
    } else if age < 2592000 {
        format!("{}d", age / 86400)
    } else {
        format!("{}mo", age / 2592000)
    }
}

fn query_blocking(cache: Arc<Mutex<Option<HistCache>>>, arg: &str) -> NativeOutcome {
    let hist = history_path();
    let mtime = std::fs::metadata(&hist).and_then(|m| m.modified()).ok();
    let entries = {
        let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
        match c.as_ref() {
            Some(c) if c.stamp == mtime && mtime.is_some() => c.entries.clone(),
            _ => {
                let parsed: Vec<Value> = std::fs::read_to_string(&hist)
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default();
                let entries = Arc::new(parsed);
                *c = Some(HistCache {
                    stamp: mtime,
                    entries: entries.clone(),
                });
                entries
            }
        }
    };
    if entries.is_empty() {
        return NativeOutcome::Empty;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let q = arg.trim().to_lowercase();
    let file = hist.to_string_lossy().to_string();
    let mut rows = Vec::new();
    let mut i = 0i64;
    for v in entries.iter() {
        let expression = v.get("expression").and_then(Value::as_str).unwrap_or("");
        let answer = v.get("answer").and_then(Value::as_str).unwrap_or("");
        if expression.is_empty() || answer.is_empty() {
            continue;
        }
        if !q.is_empty()
            && !expression.to_lowercase().contains(&q)
            && !answer.to_lowercase().contains(&q)
        {
            continue;
        }
        let at = v.get("at").and_then(Value::as_i64).unwrap_or(now);
        let copy = format!("printf %s {} | wl-copy", quote(answer));
        rows.push(json!({
            "id": expression,
            "title": answer,
            "subtitle": expression,
            "accessory": when(now - at),
            "group": "Calculator History",
            "exec": copy,
            "score": 90000 - i * 100,
            "actions": [
                { "title": "Copy Result", "shortcut": "↵", "exec": copy },
                { "title": "Copy Expression",
                  "exec": format!("printf %s {} | wl-copy", quote(expression)) },
                // `query` steers the launcher instead of running something, so
                // this reopens the sum in the calculator rather than closing
                // on a copy.
                { "title": "Ask Again", "query": format!("={expression}") },
                { "title": "Clear History", "exec": format!("rm -f {}", quote(&file)) },
            ],
        }));
        i += 1;
    }
    NativeOutcome::Rows(rows)
}

impl NativeExt for CalcHist {
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
