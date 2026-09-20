//! One integer orders results from every provider: `tier * 100000 + local`.
//! A port of `plugin/Rank.js`.
//!
//! The tier is the kind of match, and it always wins. `local` breaks ties
//! inside a tier and is where a provider's own bias lives, so a bias can
//! reorder equals but can never lift a weak match above a strong one.
//!
//! Two things add to the score after that clamp, and only one of them keeps
//! the rule: a pin is clamped to the row's own tier (`pins_apply`), while
//! frecency is added to the finished number (`frecency_apply`) and can carry
//! a row within 9000 of its ceiling past the next tier's floor. The script
//! does both the same way, so the difference is parity, not a port bug.

use std::sync::Arc;

use crate::model::row::Row;

pub const TIER_CALC: u32 = 9; // a calculator answer is what you asked for
pub const TIER_FORCED: u32 = 8; // a sigil said "only this provider"
pub const TIER_PREFIX: u32 = 7; // the name starts with what you typed
pub const TIER_SUBSTRING: u32 = 6;
pub const TIER_WEAK: u32 = 5; // an acronym or a keyword hit
pub const TIER_FILE: u32 = 4;
pub const TIER_WEB: u32 = 1; // always present, always last

pub const TIER_WIDTH: i64 = 100_000;

pub fn tier(name: &str) -> u32 {
    match name {
        "calc" => TIER_CALC,
        "forced" => TIER_FORCED,
        "prefix" => TIER_PREFIX,
        "substring" => TIER_SUBSTRING,
        "weak" => TIER_WEAK,
        "file" => TIER_FILE,
        "web" => TIER_WEB,
        _ => TIER_SUBSTRING,
    }
}

pub fn tier_name(tier: u32) -> &'static str {
    match tier {
        TIER_CALC => "calc",
        TIER_FORCED => "forced",
        TIER_PREFIX => "prefix",
        TIER_SUBSTRING => "substring",
        TIER_WEAK => "weak",
        TIER_FILE => "file",
        TIER_WEB => "web",
        _ => "substring",
    }
}

/// Map the fuzzy scorer's bands onto tiers so apps and commands, scored by the
/// same function, mean the same thing.
pub fn tier_for_fuzzy(fuzzy: i64) -> u32 {
    if fuzzy >= 9500 {
        TIER_PREFIX // 10000 name prefix, 9500 id prefix
    } else if fuzzy >= 6000 {
        TIER_SUBSTRING // 8000 name infix, 7600 id infix, 6000 haystack
    } else {
        TIER_WEAK // 5000/4600 acronym, 4000 fallback
    }
}

pub fn local_for_fuzzy(fuzzy: i64) -> i64 {
    ((fuzzy - 4000) as f64 * 16.666).round().clamp(0.0, 99999.0) as i64
}

pub fn score(tier: u32, local: i64, bias: i64) -> i64 {
    tier as i64 * TIER_WIDTH + (local + bias).clamp(0, 99999)
}

/// The total order as a key — `sort_by_cached_key` computes it once per row,
/// so the lowercase happens n times instead of n·log n comparisons.
pub fn sort_key(row: &Row) -> (std::cmp::Reverse<i64>, String) {
    (std::cmp::Reverse(row.score), row.title.to_lowercase())
}

/// Merge every provider's bucket into one ranked list.
///
/// The web row is a fallback, so it is dropped whenever anything real
/// matched. `mode` forces one provider, and a forced web row survives.
pub fn merge(buckets: &[(&str, &[Arc<Row>])], mode: &str, limit: usize) -> Vec<Arc<Row>> {
    let mut rows: Vec<Arc<Row>> = Vec::new();
    for (_, bucket) in buckets {
        // Arc bumps, not row copies: the bucket keeps its rows and the
        // merged list borrows them.
        rows.extend(bucket.iter().cloned());
    }

    let has_real = rows.iter().any(|r| r.provider_id != "web");
    if has_real && mode != "web" {
        rows.retain(|r| r.provider_id != "web");
    }

    rows.sort_by_cached_key(|r| sort_key(r));
    if limit > 0 {
        rows.truncate(limit);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_always_wins() {
        assert!(score(TIER_PREFIX, 0, 0) > score(TIER_SUBSTRING, 99999, 0));
        assert!(score(TIER_WEB, 99999, 0) < score(TIER_FILE, 0, 0));
    }

    #[test]
    fn merge_drops_web_when_real_exists() {
        let mut web = Row::new("w", "web");
        web.title = "Search".into();
        let mut real = Row::new("r", "file");
        real.title = "report".into();
        real.score = score(TIER_FILE, 10, 0);
        let rows = merge(
            &[("web", &[Arc::new(web)]), ("file", &[Arc::new(real)])],
            "",
            0,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider_id, "file");
    }
}
