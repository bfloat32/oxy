//! How often each model has answered — a ledger, not a score.
//!
//! jcode keeps one of these per route (`MODEL_USAGE.md`), and the useful part
//! is how careful its definition is: a *tracked turn*, never task success, and
//! absent fields omitted rather than zeroed. This is the small version: a
//! count and the last time, per model string, in
//! `~/.local/state/omarchy/oxy-model-usage.json`.
//!
//! It is what lets `/stats` say which backend the launcher actually uses, and
//! what a future model picker would sort by.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub models: HashMap<String, ModelUse>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ModelUse {
    #[serde(default)]
    pub count: u64,
    #[serde(default)]
    pub last_used: u64,
}

impl Usage {
    /// One completed turn on this model. `now_ms` is passed in rather than
    /// read here, so a test can pin it.
    pub fn record(&mut self, model: &str, now_ms: u64) {
        if model.trim().is_empty() {
            return;
        }
        let entry = self.models.entry(model.to_string()).or_default();
        entry.count += 1;
        entry.last_used = now_ms;
    }

    /// The model used most, for a one-line summary. Ties break on the more
    /// recent one, so a freshly switched model is named straight away.
    pub fn busiest(&self) -> Option<(&str, &ModelUse)> {
        self.models
            .iter()
            .max_by_key(|(_, u)| (u.count, u.last_used))
            .map(|(k, u)| (k.as_str(), u))
    }

    /// `/stats` says this, or nothing when no turn has completed yet.
    pub fn summary(&self) -> String {
        match self.busiest() {
            None => String::new(),
            Some((model, u)) => {
                let turns = if u.count == 1 { "turn" } else { "turns" };
                format!("{model} — {} {turns}", u.count)
            }
        }
    }
}

pub fn load(path: &Path) -> (Usage, Vec<PathBuf>) {
    let mut moved = Vec::new();
    let usage = crate::support::store::read_json::<Usage>(path, &mut moved).unwrap_or_default();
    (usage, moved)
}

pub fn save(path: &Path, usage: &Usage) {
    let text = serde_json::to_string(usage).unwrap_or_default();
    let _ = super::write_atomic(path, &text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("oxy-usage-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_turn_is_counted_and_timed() {
        let mut usage = Usage::default();
        usage.record("Local · llama3.2", 1_000);
        usage.record("Local · llama3.2", 2_000);
        usage.record("Claude", 3_000);
        assert_eq!(usage.models["Local · llama3.2"].count, 2);
        assert_eq!(usage.models["Local · llama3.2"].last_used, 2_000);
        assert_eq!(usage.models["Claude"].count, 1);
        assert_eq!(usage.summary(), "Local · llama3.2 — 2 turns");
        // An empty model name is not a model.
        usage.record("  ", 4_000);
        assert_eq!(usage.models.len(), 2);
    }

    #[test]
    fn one_turn_reads_as_a_turn() {
        let mut usage = Usage::default();
        usage.record("Claude", 1);
        assert_eq!(usage.summary(), "Claude — 1 turn");
        assert_eq!(Usage::default().summary(), "");
    }

    #[test]
    fn a_round_trip_through_the_file() {
        let d = dir("round");
        let path = d.join("oxy-model-usage.json");
        let mut usage = Usage::default();
        usage.record("Claude", 42);
        save(&path, &usage);

        let (back, moved) = load(&path);
        assert!(moved.is_empty());
        assert_eq!(back.models["Claude"].count, 1);
        assert_eq!(back.models["Claude"].last_used, 42);

        // A corrupt file is moved aside like every other state file.
        std::fs::write(&path, "{oops").unwrap();
        let (empty, moved) = load(&path);
        assert!(empty.models.is_empty());
        assert_eq!(moved, vec![d.join("oxy-model-usage.corrupt")]);
        std::fs::remove_dir_all(&d).ok();
    }
}
