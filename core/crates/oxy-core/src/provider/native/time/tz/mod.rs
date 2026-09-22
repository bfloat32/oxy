//! `tz` — the timezone answer: clocks across configured zones, a named-zone
//! lookup over the full IANA list, and the `timegrid` cells. Ported from
//! `bin/oxy-timezone` (+ `oxy-timezone-plan`) onto `jiff`, which reads the
//! same `/usr/share/zoneinfo` on Linux and bundles tzdb on Windows.
//!
//! `jq`, `date`, `timedatectl`, `iconv`, `sed`, `grep` and `python3` in the
//! script are library calls here; the strings on the wire are the same
//! strings, so a case file written against the shell still describes this.

mod grid;
mod names;
mod when;
mod zones;

use std::future::Future;
use std::pin::Pin;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::settings::Settings;

use zones::Board;

#[derive(Default)]
pub struct Tz;

/// The wrapping a question arrives in: `tz:` is reached through the `time`
/// alias, so `time:what time is it in tokyo` is typed more often than
/// `tz:tokyo`. Stripped iteratively because the wrappings chain — "what
/// time is it in …" is "what time is it" + "in". `None` is the script's
/// `exit 0`: the query was wrapping and nothing else.
const WRAPPING: &[&str] = &[
    "what time is it",
    "what time it is",
    "what is the time",
    "what's the time",
    "whats the time",
    "what time",
    "current time",
    "time now",
    "the time",
    "que horas sao",
    "que horas são",
    "hora",
    "time",
    "clock",
    "in",
    "at",
    "for",
    "on",
];

fn unwrap_question(mut trimmed: String) -> Option<String> {
    loop {
        let before = trimmed.clone();
        let lower = trimmed.to_lowercase();
        for prefix in WRAPPING {
            // `${trimmed,,} == "$prefix" || == "$prefix "*` — the word, or
            // the word and a space and more.
            if lower == *prefix
                || lower
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with(' '))
            {
                trimmed = trimmed[prefix.len()..]
                    .trim_start_matches(names::ws)
                    .to_string();
                break;
            }
        }
        if trimmed == before {
            return Some(trimmed);
        }
        if trimmed.is_empty() {
            return None;
        }
    }
}

/// `main` — the script's tail end, top to bottom, with `exit 0` as
/// `NativeOutcome` and `fail_row` as the row it always was.
fn answer(arg: &str, settings: &Settings) -> NativeOutcome {
    let board = Board::load(settings);

    let mut trimmed = arg.trim_matches(names::ws).to_string();
    if trimmed.ends_with('?') {
        trimmed.pop();
        trimmed = trimmed.trim_end_matches(names::ws).to_string();
    }
    let Some(trimmed) = unwrap_question(trimmed) else {
        return NativeOutcome::Empty;
    };

    let now = || Timestamp::now().as_second();

    // Nothing typed: the whole board.
    if trimmed.is_empty() {
        return NativeOutcome::Rows(zones::column(now(), 95000, "", &board));
    }

    // A Discord timestamp, pasted back in.
    if let Some(stamp) = when::discord_stamp(&trimmed) {
        return NativeOutcome::Rows(zones::column(stamp, 95000, "", &board));
    }

    // `name:place` pairs are a meeting, not a lookup — one pair counts too,
    // because the smallest way to ask is to name one person. The day grid
    // answers "when can all of us talk", which a column of clocks cannot.
    if !when::pair_starts(&trimmed).is_empty() {
        let mut people: Vec<(String, String)> = Vec::new();
        for seg in when::pair_segments(&trimmed) {
            if seg.is_empty() {
                continue;
            }
            // `${pair%%:*}` / `${pair#*:}` — no colon leaves both as the
            // whole segment, which the place probe then fails on purpose.
            let (who, place) = match seg.split_once(':') {
                Some((w, p)) => (w, p),
                None => (seg, seg),
            };
            let place = place.trim_matches(names::ws);
            if who.is_empty() {
                continue;
            }
            if place.is_empty() {
                // `me:` with nothing after it puts you in the grid — being
                // in the meeting is something you ask for.
                if matches!(
                    who.to_lowercase().as_str(),
                    "me" | "i" | "local" | "here" | "home"
                ) {
                    people.push((who.to_string(), board.local.clone()));
                }
                continue;
            }
            // The last word may be the start of the next name that has not
            // got its colon yet, so the probe drops trailing words until a
            // zone resolves: while `john:tokyo m` is being typed, John is
            // still in Tokyo.
            let mut probe = place;
            let zone = loop {
                if probe.is_empty() {
                    break None;
                }
                if let Some(z) = names::resolve_zone(probe, &board) {
                    break Some(z);
                }
                match probe.rfind(names::ws) {
                    Some(p) => probe = probe[..p].trim_end_matches(names::ws),
                    None => break None,
                }
            };
            let Some(zone) = zone else { continue };
            people.push((who.to_string(), zone));
        }

        // Naming one person is a comparison with you, so you are in it —
        // unless they are in your zone, where a second identical row says
        // nothing.
        if people.len() == 1 && people[0].1 != board.local {
            people.insert(0, ("you".to_string(), board.local.clone()));
        }
        if !people.is_empty() {
            let names_v: Vec<String> = people.iter().map(|(n, _)| n.clone()).collect();
            let zones_v: Vec<String> = people.iter().map(|(_, z)| z.clone()).collect();
            return match grid::draw(&names_v, &zones_v, &board.local) {
                Some(row) => NativeOutcome::Rows(vec![row]),
                None => NativeOutcome::Empty,
            };
        }
        // Nobody placed: fall through. The lookup below reports an unknown
        // place perfectly well, and a second error here fired in the middle
        // of typing — `tz: john:tokyo m` is a sentence still being written.
    }

    // A bare unix timestamp, in the range that is a plausible date rather
    // than a quantity somebody meant to convert.
    if let Some(stamp) = when::bare_stamp(&trimmed) {
        return NativeOutcome::Rows(zones::column(stamp, 95000, "", &board));
    }

    // Anything that opens a Discord timestamp and does not close one is a
    // paste in progress or a typo, and either way there is no answer yet —
    // silence for the paste, a row for the typo.
    if trimmed.contains("<t:") {
        if when::partial_discord(&trimmed) {
            return NativeOutcome::Empty;
        }
        return NativeOutcome::Rows(zones::fail_row(
            "Not a Discord timestamp",
            "Expected <t:1735689600:F>",
        ));
    }

    // An offset that is not a whole number of hours: no Etc zone can carry
    // it and rounding would answer half an hour out.
    if when::minute_offset(&trimmed) {
        return NativeOutcome::Rows(zones::fail_row(
            &format!("No zone is exactly \"{trimmed}\""),
            "Offsets come in whole hours here: try the city, such as Kolkata for +05:30",
        ));
    }

    let dst_zone;
    let mut origin = String::new();
    let parsed;

    if let Some((head, tail)) = when::split_joiner(&trimmed) {
        let Some(dst) = names::resolve_zone(&tail, &board) else {
            return NativeOutcome::Rows(zones::fail_row(
                &format!("No timezone matches \"{tail}\""),
                "Try a city, a region, or a label from oxy.json",
            ));
        };
        dst_zone = dst;

        // "3pm in tokyo in london" says "in" twice and the split took the
        // last one, leaving "3pm in tokyo" as the head — the same sentence
        // as "3pm tokyo" once the leftover joining word comes out.
        let mut w = when::parse_when(&head, &board);
        if w.is_none() {
            let head = when::contract_joiner(&head);
            w = when::parse_when(&head, &board);
            if w.is_none() {
                return NativeOutcome::Rows(zones::fail_row(
                    &format!("No timezone matches \"{head}\""),
                    "Expected a time, a zone, or both",
                ));
            }
        }
        parsed = w.unwrap_or_default();

        // "tokyo to london" is a comparison, not a conversion: there is no
        // time in it to convert, and a London clock that never mentions
        // Tokyo is a true row answering a different question.
        if parsed.time.is_empty() && !parsed.zone.is_empty() && parsed.zone != dst_zone {
            let names_v = vec![
                names::label_for_zone(&parsed.zone, &board),
                names::label_for_zone(&dst_zone, &board),
            ];
            let zones_v = vec![parsed.zone.clone(), dst_zone.clone()];
            return match grid::draw(&names_v, &zones_v, &board.local) {
                Some(row) => NativeOutcome::Rows(vec![row]),
                None => NativeOutcome::Empty,
            };
        }
    } else {
        let Some(w) = when::parse_when(&trimmed, &board) else {
            // Several places at once is the other way this question gets
            // asked.
            if let Some(places) = when::parse_places(&trimmed, &board) {
                let names_v: Vec<String> = places.iter().map(|(l, _)| l.clone()).collect();
                let zones_v: Vec<String> = places.into_iter().map(|(_, z)| z).collect();
                return match grid::draw(&names_v, &zones_v, &board.local) {
                    Some(row) => NativeOutcome::Rows(vec![row]),
                    None => NativeOutcome::Empty,
                };
            }
            return NativeOutcome::Rows(zones::fail_row(
                &format!("No timezone matches \"{trimmed}\""),
                "Try a city, a region, or a label from oxy.json",
            ));
        };
        // Without an explicit "in", a named zone is where the answer goes
        // and the time is read as yours.
        dst_zone = w.zone.clone();
        parsed = when::When {
            time: w.time,
            zone: String::new(),
        };
    }

    let stamp;
    if !parsed.time.is_empty() {
        let spec = when::normalize_time(&parsed.time);
        let (zone, suffix) = if parsed.zone.is_empty() {
            (board.local.clone(), " your time".to_string())
        } else {
            (parsed.zone.clone(), format!(" in {}", parsed.zone))
        };
        let tz = TimeZone::get(&zone).unwrap_or(TimeZone::UTC);
        let Some(s) = when::eval_spec(&spec, &tz) else {
            return NativeOutcome::Rows(zones::fail_row(
                &format!("Not a time \"{}\"", parsed.time),
                "Try 3pm, 15:30, or 9 am",
            ));
        };
        stamp = s;
        let clock = Timestamp::from_second(s)
            .ok()
            .map(|t| t.to_zoned(tz).strftime("%H:%M").to_string())
            .unwrap_or_default();
        origin = format!("{clock}{suffix}");
    } else {
        stamp = now();
    }

    if dst_zone.is_empty() {
        NativeOutcome::Rows(zones::column(stamp, 95000, "", &board))
    } else {
        NativeOutcome::Rows(zones::hero_and_column(&dst_zone, stamp, &origin, &board))
    }
}

impl NativeExt for Tz {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move { answer(&ctx.arg, &ctx.settings) })
    }
}
