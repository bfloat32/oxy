//! Pins: the rows a user has marked, kept in `oxy-state.json`. A pin is a
//! fixed lift inside the row's tier — it beats frecency, it never beats a
//! better tier.

use std::collections::HashMap;
use std::sync::Arc;

use crate::model::row::Row;

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

/// `pinned` is written on every row whose mark differs, not only the pinned
/// ones: a mark set once and never cleared would stay on a row after the pin
/// came off. Returns whether any score moved (a flag-only write is not a
/// reorder).
pub fn pins_apply(rows: &mut [Arc<Row>], pins: &Pins) -> bool {
    let mut changed = false;
    for row in rows.iter_mut() {
        if row.key.is_empty() {
            continue;
        }
        let want = pin_has(pins, &row.key);
        // The common case — an unpinned row staying unpinned — writes nothing
        // and clones nothing.
        if !want && !row.pinned {
            continue;
        }
        let row = Arc::make_mut(row);
        row.pinned = want;
        if !want {
            continue;
        }
        let tier = row.score.div_euclid(crate::support::rank::TIER_WIDTH);
        row.score = row
            .score
            .saturating_add(PIN_BOOST)
            .min(tier * crate::support::rank::TIER_WIDTH + (crate::support::rank::TIER_WIDTH - 1));
        changed = true;
    }
    changed
}
