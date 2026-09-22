//! The day grid — `bin/oxy-timezone-plan` inline, so `tz:john:tokyo
//! maria:spain` needs no python on PATH. A column of times answers "what
//! time is it for John"; it does not answer "when can the four of us talk",
//! which is the only reason anyone types four names. So this emits one row
//! carrying the whole day for everybody, and the view shades each hour by
//! whether it is reasonable where that person is.

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::{Value, json};

use crate::support::quote::quote;

/// What an hour is worth, where somebody is. Not a preference: these are
/// the hours a person can be asked to take a call in without it being a
/// favour.
fn band(hour: i8) -> &'static str {
    if !(7..23).contains(&hour) {
        "asleep"
    } else if hour < 9 {
        "early"
    } else if hour < 18 {
        "work"
    } else {
        "evening"
    }
}

/// '5h30 ahead', '9h30 behind', 'same time' — the difference as it is.
/// Floor division ate the halves before: `total_seconds() // 3600` turned
/// India's +5:30 into '5h ahead' and rounded a negative half-hour a full
/// hour past itself, so -9:30 read '10h behind'. Minutes are part of the
/// answer for a third of the world's population.
fn delta_text(total: i64) -> String {
    if total == 0 {
        return "same time".to_string();
    }
    let (hours, minutes) = (total.abs() / 60 / 60, total.abs() / 60 % 60);
    let tail = if minutes > 0 {
        format!("{minutes:02}")
    } else {
        String::new()
    };
    format!(
        "{hours}h{tail} {}",
        if total > 0 { "ahead" } else { "behind" }
    )
}

/// The longest unbroken stretch in a sorted step list, as text: six
/// scattered hours and six in a row are different answers, and a count
/// alone hides which one it is. `when` renders a step as the home clock's
/// HH:MM; the end of a run is one step past its last hour.
fn run_of(steps: &[usize], when: &dyn Fn(usize) -> String) -> String {
    if steps.is_empty() {
        return String::new();
    }
    let mut best = (steps[0], 1usize);
    let mut run = (steps[0], 1usize);
    for w in steps.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b == a + 1 {
            run.1 += 1;
        } else {
            run = (b, 1);
        }
        if run.1 > best.1 {
            best = run;
        }
    }
    // The python leg had `best_start = start - length + 1` — the run's own
    // start minus its length minus one — which reported the window ending
    // one step after the run began. The run is `start .. start + length`.
    if best.1 == 1 {
        return when(best.0);
    }
    format!("{} to {}", when(best.0), when(best.0 + best.1))
}

/// Draw the grid for `(name, zone)` pairs — one row carrying twenty-four
/// shaded hours per person. `None` is the script printing nothing: nobody
/// placed.
pub(super) fn draw(names: &[String], zones: &[String], home: &str) -> Option<Value> {
    if names.is_empty() || names.len() != zones.len() {
        return None;
    }
    let home_tz = TimeZone::get(home).unwrap_or(TimeZone::UTC);

    // The grid runs over the next twenty-four hours from the top of this
    // hour, rather than over "today", because a meeting is scheduled forward
    // and a grid that starts at midnight spends its first third on hours
    // nobody can use any more.
    let now_z = Timestamp::now().to_zoned(home_tz.clone());
    let base = now_z
        .date()
        .at(now_z.hour(), 0, 0, 0)
        .to_zoned(home_tz.clone())
        .ok()?;
    let base_ts = base.timestamp().as_second();

    let mut people: Vec<Value> = Vec::new();
    for (who, zone) in names.iter().zip(zones.iter()) {
        let Ok(tz) = TimeZone::get(zone) else {
            continue;
        };
        let mut cells = Vec::with_capacity(24);
        for step in 0..24i64 {
            let Ok(moment) = base.checked_add(jiff::Span::new().hours(step)) else {
                return None;
            };
            let there = moment.with_time_zone(tz.clone());
            let hour = there.hour();
            // The day only matters when it is not the same as the reader's,
            // and it is the thing people get wrong.
            let day = if there.date() != moment.date() {
                there.strftime("%a").to_string()
            } else {
                String::new()
            };
            cells.push(json!({
                "hour": hour,
                "label": there.strftime("%H:%M").to_string(),
                "band": band(hour),
                "day": day,
            }));
        }

        // The offset as it is now, not as it is at the end of the window —
        // a DST transition inside the window is already drawn where it
        // lands in the cells above.
        let there_now = base.with_time_zone(tz);
        let home_now = base.with_time_zone(home_tz.clone());
        let offset =
            i64::from(there_now.offset().seconds()) - i64::from(home_now.offset().seconds());

        people.push(json!({
            "name": who,
            "zone": zone,
            "city": zone.rsplit('/').next().unwrap_or(zone).replace('_', " "),
            "delta": delta_text(offset),
            "cells": cells,
        }));
    }
    if people.is_empty() {
        return None;
    }

    // The overlap, reported rather than judged. An earlier version said "no
    // hour suits everyone", which is a verdict about other people's days
    // that this program is in no position to make: plenty of people take a
    // call at eight in the evening. What it can say is who is awake when,
    // and let the reader decide what to ask of whom.
    let mut awake = Vec::new();
    let mut working = Vec::new();
    for step in 0..24usize {
        let all_awake = people.iter().all(|p| p["cells"][step]["band"] != "asleep");
        let all_work = people.iter().all(|p| p["cells"][step]["band"] == "work");
        if all_awake {
            awake.push(step);
        }
        if all_work {
            working.push(step);
        }
    }

    let when = |step: usize| {
        base.checked_add(jiff::Span::new().hours(step as i64))
            .map(|z| z.strftime("%H:%M").to_string())
            .unwrap_or_default()
    };

    let (headline, detail) = if awake.is_empty() {
        (
            "No hour with everyone awake".to_string(),
            "The closest you get is asking someone early or late".to_string(),
        )
    } else {
        let plural = if awake.len() > 1 { "s" } else { "" };
        let mut detail = format!("Your {}", run_of(&awake, &when));
        if !working.is_empty() {
            detail = format!("{detail}   ·   all at work {}", run_of(&working, &when));
        }
        (
            format!("Everyone awake for {} hour{plural}", awake.len()),
            detail,
        )
    };

    let steps: Vec<usize> = if awake.is_empty() {
        (0..24).collect()
    } else {
        awake.clone()
    };
    let stamps: Vec<i64> = steps.iter().map(|s| base_ts + *s as i64 * 3600).collect();

    let mut row = json!({
        "id": "tz-plan",
        "title": headline,
        "subtitle": people
            .iter()
            .map(|p| p["name"].as_str().unwrap_or(""))
            .collect::<Vec<_>>()
            .join(", "),
        "detail": detail,
        "view": "timegrid",
        "score": 99000,
        "people": people,
        "startsAt": base_ts,
        "best": working,
        "ok": awake,
        "exec": "",
        "actions": [],
    });

    if let Some(first) = stamps.first() {
        row["exec"] = json!(format!(
            "printf %s {} | wl-copy",
            quote(&format!("<t:{first}:t>"))
        ));
        let joined = stamps[..stamps.len().min(8)]
            .iter()
            .map(|s| format!("<t:{s}:t>"))
            .collect::<Vec<_>>()
            .join(" ");
        row["actions"] = json!([
            {
                "title": "Copy the Time",
                "shortcut": "↵",
                "exec": format!("printf %s {} | wl-copy", quote(&format!("<t:{first}:t>"))),
            },
            {
                "title": "Copy Date and Time",
                "exec": format!("printf %s {} | wl-copy", quote(&format!("<t:{first}:F>"))),
            },
            {
                "title": "Copy Every Good Hour",
                "exec": format!("printf %s {} | wl-copy", quote(&joined)),
            },
        ]);
    }
    Some(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_delta_keeps_its_half_hours() {
        assert_eq!(delta_text(0), "same time");
        assert_eq!(delta_text(3600), "1h ahead");
        assert_eq!(delta_text(-3600), "1h behind");
        assert_eq!(delta_text(19800), "5h30 ahead");
        assert_eq!(delta_text(-34200), "9h30 behind");
        assert_eq!(delta_text(1800), "0h30 ahead");
    }

    #[test]
    fn runs_report_their_own_window() {
        let when = |s: usize| format!("{s:02}:00");
        assert_eq!(run_of(&[3, 4, 5], &when), "03:00 to 06:00");
        assert_eq!(run_of(&[2, 3, 4, 10, 11], &when), "02:00 to 05:00");
        assert_eq!(run_of(&[9], &when), "09:00");
        assert_eq!(run_of(&[], &when), "");
        assert_eq!(run_of(&[0, 2, 4], &when), "00:00");
    }

    #[test]
    fn the_grid_is_one_row_of_people() {
        let row = draw(
            &["you".to_string(), "maria".to_string()],
            &["UTC".to_string(), "Asia/Tokyo".to_string()],
            "UTC",
        )
        .expect("utc and tokyo resolve");
        assert_eq!(row["id"], "tz-plan");
        assert_eq!(row["view"], "timegrid");
        assert_eq!(row["subtitle"], "you, maria");
        assert_eq!(row["people"].as_array().unwrap().len(), 2);
        assert_eq!(row["people"][0]["name"], "you");
        assert_eq!(row["people"][1]["city"], "Tokyo");
        assert_eq!(row["people"][1]["delta"], "9h ahead");
        assert_eq!(row["people"][0]["cells"].as_array().unwrap().len(), 24);
        assert_eq!(row["actions"].as_array().unwrap().len(), 3);
        // A single person at UTC is awake the 16 hours outside 0–6 and 23,
        // and at work the 9 hours from 9 to 17.
        let solo = draw(&["me".to_string()], &["UTC".to_string()], "UTC").unwrap();
        assert_eq!(solo["ok"].as_array().unwrap().len(), 16);
        assert_eq!(solo["best"].as_array().unwrap().len(), 9);
    }

    #[test]
    fn nobody_placed_is_no_answer() {
        assert!(draw(&[], &[], "UTC").is_none());
        assert!(draw(&["x".to_string()], &[], "UTC").is_none());
    }
}
