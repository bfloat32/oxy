//! Extension answers held in memory — a port of `plugin/Cache.js`.
//!
//! Entries are keyed by the exact question that was asked (the command for a
//! script, the spelled-out question for a socket or native provider), so two
//! queries that differ only in a filter never share an answer.

use std::collections::{HashMap, VecDeque};

use crate::extension::Extension;
use crate::row::SharedRows;

/// Bounds, because a daemon stays up for days and a launcher that leaks one
/// entry per distinct query is a launcher that leaks.
const MAX_PER_PROVIDER: usize = 16;
const MAX_PROVIDERS: usize = 48;

struct Entry {
    // Shared, not copied: a hit hands the caller an `Arc` bump, not a deep
    // clone of every row — the rows are read-only after `put`. Built rows,
    // not raw JSON: the conversion runs once at fill time instead of once
    // per hit.
    rows: SharedRows,
    expires: u64,
    /// Fingerprint of the extension fields `to_row` reads. `Shared` — and so
    /// this cache — outlives a registry reload, and rows built under an old
    /// definition must not answer for the new one.
    stamp: u64,
}

/// Cheap fingerprint of the `Extension` fields a row build consults — id,
/// group/subtitle/glyph/view fallbacks, tier, and the row cap. Computed once
/// per worker at spawn; extension files are trusted local config, so
/// collision resistance beyond `DefaultHasher` buys nothing.
pub fn ext_stamp(ext: &Extension) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ext.id.hash(&mut h);
    ext.title.hash(&mut h);
    ext.subtitle.hash(&mut h);
    ext.glyph.hash(&mut h);
    ext.view.hash(&mut h);
    ext.tier.hash(&mut h);
    ext.max_rows.hash(&mut h);
    h.finish()
}

#[derive(Default)]
struct Bucket {
    /// LRU order: front is most recent.
    keys: VecDeque<String>,
    entries: HashMap<String, Entry>,
}

impl Bucket {
    fn touch(&mut self, key: &str) {
        if let Some(at) = self.keys.iter().position(|k| k == key)
            && at > 0
        {
            let key = self.keys.remove(at).unwrap();
            self.keys.push_front(key);
        }
    }
}

#[derive(Default)]
pub struct Cache {
    store: HashMap<String, Bucket>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Cache {
    /// The built rows, or `None` for a miss, an expired entry, or a stamp
    /// mismatch. An expired entry is kept rather than dropped: `get_stale`
    /// still serves it while its replacement is fetched.
    pub fn get(&mut self, provider: &str, key: &str, stamp: u64) -> Option<SharedRows> {
        self.get_stale(provider, key, stamp).filter(|_| {
            self.store
                .get(provider)
                .and_then(|b| b.entries.get(key))
                .is_some_and(|e| e.expires > now_ms())
        })
    }

    /// The same lookup without the expiry check. A second-past-ttl answer is
    /// still the best thing to draw while the real one is being fetched.
    pub fn get_stale(&mut self, provider: &str, key: &str, stamp: u64) -> Option<SharedRows> {
        let bucket = self.store.get_mut(provider)?;
        let entry = bucket.entries.get(key)?;
        if entry.stamp != stamp {
            return None;
        }
        let rows = entry.rows.clone();
        bucket.touch(key);
        Some(rows)
    }

    /// `ttl_ms == 0` means the extension did not ask to be cached, so nothing
    /// is stored. That is the default on purpose: an answer about live state
    /// is wrong the moment the user acts on it.
    pub fn put(&mut self, provider: &str, key: &str, rows: SharedRows, ttl_ms: u64, stamp: u64) {
        if ttl_ms == 0 || key.is_empty() {
            return;
        }
        if self.store.len() >= MAX_PROVIDERS && !self.store.contains_key(provider) {
            self.store.clear();
        }
        let bucket = self.store.entry(provider.to_string()).or_default();
        bucket.entries.insert(
            key.to_string(),
            Entry {
                rows,
                expires: now_ms() + ttl_ms,
                stamp,
            },
        );
        bucket.touch(key);
        while bucket.keys.len() > MAX_PER_PROVIDER {
            if let Some(evicted) = bucket.keys.pop_back() {
                bucket.entries.remove(&evicted);
            }
        }
    }

    /// How many answers are held, for the `/stats` row.
    pub fn entries(&self) -> usize {
        self.store.values().map(|b| b.entries.len()).sum()
    }

    pub fn drop_provider(&mut self, provider: &str, key: Option<&str>) {
        match key {
            None => {
                self.store.remove(provider);
            }
            Some(key) => {
                if let Some(bucket) = self.store.get_mut(provider) {
                    bucket.entries.remove(key);
                    if let Some(at) = bucket.keys.iter().position(|k| k == key) {
                        bucket.keys.remove(at);
                    }
                }
            }
        }
    }
}
