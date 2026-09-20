//! The answer to "is the software this extension needs actually here" — a
//! port of `plugin/Availability.js`.
//!
//! Keyed by the check itself and not by the extension id, so the four
//! keywords that all ask `command -v gh` cost one check between them.

use std::collections::HashMap;

/// A passing check is never re-run: software rarely goes away mid-session.
/// A failing one is re-run when somebody types the keyword, at most this
/// often, because writing an ~/.ssh/config should make a keyword real without
/// restarting the daemon.
const RECHECK_MS: u64 = 15_000;
const MAX_ENTRIES: usize = 128;

struct Entry {
    ok: bool,
    at: u64,
}

#[derive(Default)]
pub struct Availability {
    store: HashMap<String, Entry>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Availability {
    /// `None` means "no answer yet, go and ask". `Some` is an answer.
    pub fn get(&self, check: &str) -> Option<bool> {
        if check.is_empty() {
            return Some(true);
        }
        match self.store.get(check) {
            None => None,
            Some(e) if e.ok => Some(true),
            Some(e) if now_ms() - e.at < RECHECK_MS => Some(false),
            Some(_) => None,
        }
    }

    /// (entries, passing) for the `/stats` row.
    pub fn stats(&self) -> (usize, usize) {
        (
            self.store.len(),
            self.store.values().filter(|e| e.ok).count(),
        )
    }

    pub fn put(&mut self, check: &str, ok: bool) {
        if check.is_empty() {
            return;
        }
        if !self.store.contains_key(check) && self.store.len() >= MAX_ENTRIES {
            self.store.clear();
        }
        self.store
            .insert(check.to_string(), Entry { ok, at: now_ms() });
    }
}
