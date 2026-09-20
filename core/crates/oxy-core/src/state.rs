//! The state a launcher earns: frecency, pins and recent queries.
//!
//! Two files, same as the QML launcher used: `oxy-frecency.json` (decaying
//! launch counts) and `oxy-state.json` (`{recents, pins}` — neither decays).

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::row::Row;

// ---------------------------------------------------------------- frecency

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
/// tier.
pub fn frecency_apply(rows: &mut [Row], store: &Frecency, now_ms: u64, query: &str) {
    for row in rows.iter_mut() {
        if row.key.is_empty() || NEVER_REORDER.contains(&row.provider_id.as_str()) {
            continue;
        }
        let extra = frecency_boost(store, &row.key, now_ms, query);
        if extra > 0 {
            row.score += extra;
        }
    }
}

// -------------------------------------------------------------------- pins

/// The lift, in `local` points. A pin at 20000 always wins the frecency
/// argument inside a tier, and the clamp keeps it from ever winning across
/// one: a name that starts with what you typed still beats a pinned
/// substring.
const PIN_BOOST: i64 = 20_000;

pub type Pins = HashMap<String, bool>;

pub fn pin_has(pins: &Pins, key: &str) -> bool {
    pins.get(key) == Some(&true)
}

pub fn pin_toggle(pins: &mut Pins, key: &str) {
    if pins.get(key) == Some(&true) {
        pins.remove(key);
    } else {
        pins.insert(key.to_string(), true);
    }
}

/// `pinned` is written on every row, not only the pinned ones: a mark set once
/// and never cleared would stay on a row after the pin came off.
pub fn pins_apply(rows: &mut [Row], pins: &Pins) {
    for row in rows.iter_mut() {
        if row.key.is_empty() {
            continue;
        }
        row.pinned = pin_has(pins, &row.key);
        if !row.pinned {
            continue;
        }
        let tier = row.score.div_euclid(crate::rank::TIER_WIDTH);
        row.score = row
            .score
            .saturating_add(PIN_BOOST)
            .min(tier * crate::rank::TIER_WIDTH + (crate::rank::TIER_WIDTH - 1));
    }
}

// ----------------------------------------------------------------- recents

/// The last queries that led somewhere, newest first, offered when the box is
/// empty. What is kept is the text, never the row it matched: a query is a
/// question and its answer changes.
const RECENTS_LIMIT: usize = 20;

/// A bare `file:` is a mode you are entering, not a search you made.
fn worth_keeping(text: &str) -> bool {
    let entry = text.trim();
    if entry.is_empty() {
        return false;
    }
    // `keyword:` alone
    let bytes = entry.as_bytes();
    if bytes.len() > 1
        && bytes[bytes.len() - 1] == b':'
        && entry[..entry.len() - 1].chars().enumerate().all(|(i, c)| {
            if i == 0 {
                c.is_ascii_alphabetic()
            } else {
                c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'
            }
        })
    {
        return false;
    }
    !matches!(entry, "=" | ">" | "?" | "/")
}

pub fn recents_record(list: &mut Vec<String>, text: &str) {
    if !worth_keeping(text) {
        return;
    }
    let entry = text.trim().to_string();
    if list.first() == Some(&entry) {
        return;
    }
    list.retain(|e| *e != entry);
    list.insert(0, entry);
    list.truncate(RECENTS_LIMIT);
}

// -------------------------------------------------------------- mru files

/// A provider-declared recency file. A row or an action carrying
/// `"remember": {"file": "emoji-recent", "value": "😂"}` asks the engine to
/// put the value at the top of `~/.local/state/omarchy/oxy-<file>` — the
/// write `oxy-emoji --used` did, without the script having to be on PATH.
///
/// Newest first, one value per line, exact duplicates kept once — the same
/// contract the scripts that own these files follow, so a machine running
/// both variants shares one list.
pub fn mru_record(state_dir: &Path, name: &str, value: &str, keep: usize) {
    // The name picks the file: only `a-z0-9-` reaches the path, so a provider
    // cannot write outside the state dir through it.
    let clean: String = name
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    if clean.is_empty() || value.is_empty() || keep == 0 {
        return;
    }
    let path = state_dir.join(format!("oxy-{clean}"));
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .ok()
        .map(|t| {
            t.lines()
                .map(|l| l.to_string())
                .filter(|l| !l.is_empty() && *l != value)
                .collect()
        })
        .unwrap_or_default();
    lines.insert(0, value.to_string());
    lines.truncate(keep);
    let _ = write_atomic(&path, &(lines.join("\n") + "\n"));
}

// ------------------------------------------------------------ persistence

#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    recents: Vec<String>,
    #[serde(default)]
    pins: Pins,
}

pub struct State {
    pub frecency: Frecency,
    pub recents: Vec<String>,
    pub pins: Pins,
}

impl State {
    pub fn load(frecency_path: &Path, state_path: &Path) -> State {
        let frecency = std::fs::read_to_string(frecency_path)
            .ok()
            .and_then(|t| serde_json::from_str::<Frecency>(&t).ok())
            .unwrap_or_default();

        let (recents, pins) = std::fs::read_to_string(state_path)
            .ok()
            .and_then(|t| serde_json::from_str::<StateFile>(&t).ok())
            .map(|s| {
                let recents = s
                    .recents
                    .into_iter()
                    .map(|r| r.trim().to_string())
                    .filter(|r| !r.is_empty())
                    .take(RECENTS_LIMIT)
                    .collect();
                let pins: Pins = s.pins.into_iter().filter(|(_, v)| *v).collect();
                (recents, pins)
            })
            .unwrap_or_default();

        State {
            frecency,
            recents,
            pins,
        }
    }

    /// Atomic write: the file is renamed over, so a crash mid-save cannot lose
    /// the ranking.
    pub fn save(&self, frecency_path: &Path, state_path: &Path) -> std::io::Result<()> {
        write_atomic(
            frecency_path,
            &serde_json::to_string(&self.frecency).unwrap_or_default(),
        )?;
        write_atomic(
            state_path,
            &serde_json::to_string(&StateFile {
                version: 1,
                recents: self.recents.clone(),
                pins: self.pins.clone(),
            })
            .unwrap_or_default(),
        )
    }
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}
