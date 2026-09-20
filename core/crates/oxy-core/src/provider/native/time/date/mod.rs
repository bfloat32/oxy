//! `date:` — a date typed in plain words, answered natively.
//!
//! A port of `bin/oxy-date`: "27 november 2027", "next friday", "in 90 days",
//! "christmas", "christmas 2028", "from 1 jan to today", "week 34". The bash
//! leg leant on `date -d` for the general parse; here the grammar it was
//! actually asked to parse is written out — the `looks_like_date` gate admits
//! a small set of forms and each is handled directly.
//!
//! This one answers unscoped (`always`), so it has to be quiet unless it is
//! confident. The gate is deliberately narrow: `date -d` will happily read
//! "may" as a month and "1" as a day of the current month, which would put a
//! date row on top of every search for a file called 1. A query has to look
//! like a date before it is treated as one.
//!
//! Everything here resolves to a day, never to an instant, and the maths is
//! done on day numbers — a day that gains or loses an hour cannot land the
//! relative count one short either side of a clock change.
//!
//! `cal:` asks this parser what a phrase means rather than parsing the same
//! English a second time — `resolve_span` is the in-process `--iso` mode the
//! script shelled out for, so the two keywords cannot disagree about which
//! day Christmas is.

mod grammar;
mod holidays;
mod parse;
mod render;
mod words;

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::time::days;
use crate::provider::{Ctx, NativeExt, NativeOutcome};

use self::grammar::{
    try_bounds, try_month, try_next_weekday, try_quarter, try_range, try_week, try_weekend,
};
use self::holidays::{Prefer, match_holidays};
use self::parse::{resolve_one, slash_note};
use self::render::{Spec, render};

pub struct Date;

// --------------------------------------------------------------- the answer

/// Every row the query produces, as specs — row rendering lives in
/// [`render`], so the same answer can be drawn or handed to `cal:` as a span.
fn resolve(q: &str, today: i64, today_year: i64) -> Vec<Spec> {
    for f in [
        try_week,
        try_bounds,
        try_quarter,
        try_weekend,
        try_month,
        try_range,
        try_next_weekday,
    ] {
        if let Some(specs) = f(q, today, today_year) {
            return specs;
        }
    }

    // A named day, or several: `new` is both New Year's Day and New Year's
    // Eve, and somebody halfway through typing one of them wants to see both
    // rather than whichever sorts first. The nearest leads and the rest list
    // underneath it.
    let mut named = Vec::new();
    if let Some((name, y)) = q.rsplit_once(' ')
        && y.len() == 4
        && y.chars().all(|c| c.is_ascii_digit())
        && let Ok(year) = y.parse::<i64>()
        && name.chars().any(|c| !c.is_ascii_digit())
    {
        named = match_holidays(name, year, true, Prefer::Next, today, today_year);
    }
    if named.is_empty() {
        named = match_holidays(q, today_year, false, Prefer::Next, today, today_year);
    }
    if !named.is_empty() {
        return named
            .into_iter()
            .enumerate()
            .map(|(i, (_, day, name))| Spec::Day {
                day,
                label: name.to_string(),
                lead: i == 0,
                note: None,
            })
            .collect();
    }

    if let Some(day) = resolve_one(q, Prefer::Next, today, today_year) {
        return vec![Spec::Day {
            day,
            label: String::new(),
            lead: true,
            note: slash_note(q),
        }];
    }
    Vec::new()
}

/// The `--iso` mode the calendar leg shelled out for: two day numbers, equal
/// when the answer is one day, sorted when it is a range. `cal:` asks this
/// parser what a phrase means so the two keywords cannot disagree.
pub fn resolve_span(q: &str, today: i64, today_year: i64) -> Option<(i64, i64)> {
    // An empty query answered nothing on the script leg either.
    if q.is_empty() {
        return None;
    }
    resolve(q, today, today_year).first().map(|s| match *s {
        Spec::Day { day, .. } => (day, day),
        Spec::Period { from, to, .. } => (from, to),
        Spec::Range { from, to } => (from.min(to), from.max(to)),
    })
}

impl NativeExt for Date {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move {
            // Collapse the spacing once, here, so every pattern is written
            // against single spaces.
            let q = ctx
                .arg
                .replace('\n', " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            if q.is_empty() {
                return NativeOutcome::Empty;
            }
            let today = days::today_num();
            let (today_year, _, _) = days::civil_from_days(today);
            let specs = resolve(&q, today, today_year);
            if specs.is_empty() {
                return NativeOutcome::Empty;
            }
            NativeOutcome::Rows(specs.iter().map(|s| render(s, today)).collect())
        })
    }
}
