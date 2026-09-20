//! Frecency: decaying launch counts, read from `oxy-frecency.json`. A launch
//! is worth less the older it gets, and it can only reorder a tier, never
//! cross one.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::model::row::Row;

/// Half-life in days: a launch is worth half as much a week after you made it.
const HALF_LIFE_DAYS: f64 = 7.0;
const DAY_MS: f64 = 86_400_000.0;
/// The most a boost can move a row inside its tier. It never crosses one.
const MAX_BOOST: i64 = 9000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FrecencyEntry {
    #[serde(default)]
    pub count: u64,
    #[serde(default)]
    pub last: u64,
    /// The query that chose this row: an exact repeat is the strongest hint.
    #[serde(default)]
    pub q: String,
}

impl FrecencyEntry {
    fn decayed(&self, now_ms: u64) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        let age_days = (now_ms.saturating_sub(self.last) as f64 / DAY_MS).max(0.0);
        let weight = 0.5f64.powf(age_days / HALF_LIFE_DAYS);
        // Diminishing returns on count: the tenth launch says much less than
        // the second.
        (1.0 + self.count as f64).ln() * weight
    }
}

pub type Frecency = HashMap<String, FrecencyEntry>;

fn clean(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn context_boost(entry: &FrecencyEntry, query: &str) -> f64 {
    let had = entry.q.as_str();
    let want = clean(query);
    if had.is_empty() || want.is_empty() {
        return 1.0;
    }
    if had == want {
        return 1.6;
    }
    if had.starts_with(&want) || want.starts_with(had) {
        return 1.3;
    }
    1.0
}

pub fn frecency_boost(store: &Frecency, key: &str, now_ms: u64, query: &str) -> i64 {
    let Some(entry) = store.get(key) else {
        return 0;
    };
    let score = entry.decayed(now_ms);
    if score <= 0.0 {
        return 0;
    }
    // ln(1 + count) tops out around 4.6 for a hundred launches, so scaling by
    // 2000 puts a well-used entry near the ceiling without ever reaching it.
    (score * 2000.0 * context_boost(entry, query))
        .round()
        .min(MAX_BOOST as f64) as i64
}

pub fn frecency_record(store: &mut Frecency, key: &str, now_ms: u64, query: &str) {
    let q = clean(query);
    let entry = store.entry(key.to_string()).or_default();
    entry.count += 1;
    entry.last = now_ms;
    if !q.is_empty() {
        entry.q = q;
    }
}

/// Drop what has decayed to nothing, so the file does not grow forever.
pub fn frecency_prune(store: &mut Frecency, now_ms: u64) {
    store.retain(|_, e| e.decayed(now_ms) > 0.01);
}

/// Some rows are a fixed list in a deliberate order, not a set of things you
/// launch. A destructive action drifting to the top of the list, under the
/// cursor, is the wrong reward for having used it.
const NEVER_REORDER: &[&str] = &["actions"];

/// Rows arrive already scored by match quality. This only reorders within a
/// tier. Returns whether any score moved, so the caller can skip a re-sort
/// when nothing did.
pub fn frecency_apply(rows: &mut [Arc<Row>], store: &Frecency, now_ms: u64, query: &str) -> bool {
    let mut changed = false;
    for row in rows.iter_mut() {
        if row.key.is_empty() || NEVER_REORDER.contains(&row.provider_id.as_str()) {
            continue;
        }
        let extra = frecency_boost(store, &row.key, now_ms, query);
        if extra > 0 {
            // PERF: `make_mut` clones only the boosted row — the bucket keeps
            // its copy untouched, and unboosted rows stay shared.
            Arc::make_mut(row).score += extra;
            changed = true;
        }
    }
    changed
}
