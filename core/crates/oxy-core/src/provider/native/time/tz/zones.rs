//! The board and the row shapes. The board is your zone plus the configured
//! `(label, zone)` pairs, in the order they were written — order is a choice
//! the user made, so it is never re-sorted. The rows are the script's: a
//! `hero` for the single conversion that is the whole question, the `zones`
//! column under it, the Discord timestamp actions every instant earns, and
//! the `fail` row that says what could not be read.
//!
//! `timedatectl`, `date` and `jq -cn` in the script are jiff and serde_json
//! here; the strings on the wire are the same strings.

use std::collections::BTreeSet;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::{Value, json};

use super::names;
use crate::provider::native::time::days;
use crate::provider::native::util::shq;
use crate::settings::Settings;
use crate::support::quote::quote;

/// What `tz:` knows without being asked: your zone first, then the
/// configured pairs.
pub(super) struct Board {
    pub local: String,
    pub pairs: Vec<(String, String)>,
}

impl Board {
    pub fn load(settings: &Settings) -> Board {
        // `timedatectl show -p Timezone`, then the `/etc/localtime` link,
        // then UTC — here: whatever jiff decided the system zone is, UTC
        // when it cannot say.
        let local = TimeZone::system().iana_name().unwrap_or("UTC").to_string();
        let mut pairs = configured(settings);
        if pairs.is_empty() {
            // Cities people have colleagues in. UTC was in here and is not a
            // place anybody is: it is a reference for machines, and a row of
            // it in a list of cities is a line to skip past every time.
            // `tz:utc` still answers.
            pairs = [
                ("San Francisco", "America/Los_Angeles"),
                ("New York", "America/New_York"),
                ("London", "Europe/London"),
                ("Berlin", "Europe/Berlin"),
                ("Tokyo", "Asia/Tokyo"),
            ]
            .iter()
            .map(|(l, z)| ((*l).to_string(), (*z).to_string()))
            .collect();
        }

        // `settings:` collects zones as one comma-separated line, which the
        // script leg receives as OXY_ZONES. Names resolve the loose way a
        // `tz:` query does, and the setting wins over the file for the usual
        // reason: the thing just typed means more than the file edited last
        // month. Empty stays empty — a cleared field is not an instruction.
        let env = settings
            .settings_for("tz")
            .and_then(|m| m.get("zones"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !env.is_empty() {
            let probe = Board {
                local: local.clone(),
                pairs: pairs.clone(),
            };
            let mut resolved = Vec::new();
            for name in env.split(',') {
                let name = name.trim_matches(names::ws);
                if name.is_empty() {
                    continue;
                }
                if let Some(zone) = names::resolve_zone(name, &probe) {
                    resolved.push((name.to_string(), zone));
                }
            }
            if !resolved.is_empty() {
                pairs = resolved;
            }
        }
        Board { local, pairs }
    }
}

/// `(.timezones // [])[]? | select(.zone)` — a `{label, zone}` pair in
/// written order, the label defaulting to the zone. A zone that is not a
/// real name drops out, which is the `[[ -e /usr/share/zoneinfo/… ]]` test.
fn configured(settings: &Settings) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(list) = settings.raw.get("timezones").and_then(Value::as_array) else {
        return out;
    };
    for entry in list {
        let Some(zone) = entry.get("zone").and_then(Value::as_str) else {
            continue;
        };
        // jq's `select(.zone)` passes "" too; the shell's `-z` drops it, and
        // `-e` drops whatever is not a real zone name.
        if zone.is_empty() || !known_zone(zone) {
            continue;
        }
        let label = match entry.get("label") {
            Some(Value::String(s)) => s.clone(),
            // `.label // .zone`: null, false and absent fall to the zone; a
            // number or a bool renders the way jq renders a scalar.
            Some(v) if !v.is_null() && *v != Value::Bool(false) => v.to_string(),
            _ => zone.to_string(),
        };
        out.push((label, zone.to_string()));
    }
    out
}

/// `[[ -e /usr/share/zoneinfo/$zone ]]` — the exact spelling has to name a
/// zone. The case-folded hits are `resolve_zone`'s looseness, not the
/// file's.
fn known_zone(zone: &str) -> bool {
    if names::all_zones().iter().any(|z| z == zone) {
        return true;
    }
    TimeZone::get(zone)
        .ok()
        .and_then(|tz| tz.iana_name().map(str::to_string))
        .as_deref()
        == Some(zone)
}

/// Offset in minutes east of UTC at the instant in question, so a DST
/// boundary between now and then is accounted for rather than assumed away.
fn offset_minutes(zone: &str, stamp: i64) -> Option<i64> {
    let tz = TimeZone::get(zone).ok()?;
    let ts = Timestamp::from_second(stamp).ok()?;
    Some(i64::from(ts.to_zoned(tz).offset().seconds()) / 60)
}

/// "3h ahead", "30m behind", "same time" — the difference as it is.
fn describe_shift(delta: i64) -> String {
    if delta == 0 {
        return "same time".to_string();
    }
    let word = if delta > 0 { "ahead" } else { "behind" };
    let abs = delta.abs();
    let (h, m) = (abs / 60, abs % 60);
    if m == 0 {
        format!("{h}h {word}")
    } else if h == 0 {
        format!("{m}m {word}")
    } else {
        format!("{h}h{m:02} {word}")
    }
}

/// Discord renders `<t:UNIX:STYLE>` in the reader's own zone and locale,
/// which is exactly the problem this provider solves in the other
/// direction. All nine styles are offered because the useful one depends on
/// the sentence.
fn discord_actions(stamp: i64) -> Vec<Value> {
    const STYLES: [(&str, &str); 9] = [
        ("F", "Full Date and Time"),
        ("f", "Date and Time"),
        ("R", "Relative"),
        ("t", "Time"),
        ("T", "Time with Seconds"),
        ("D", "Long Date"),
        ("d", "Short Date"),
        ("s", "Short Date and Time"),
        ("S", "Short Date and Time with Seconds"),
    ];
    STYLES
        .iter()
        .map(|(style, name)| {
            json!({
                "title": format!("Copy Discord {name} ({style})"),
                "exec": format!("printf %s {} | wl-copy", quote(&format!("<t:{stamp}:{style}>"))),
            })
        })
        .collect()
}

/// One zone at one instant. `hero` is for the single conversion that is the
/// whole question; anything else is a `zones` row for the column underneath
/// it. `origin` is the "from 15:00 in Asia/Tokyo" tail a converted time
/// earns.
#[allow(clippy::too_many_arguments)]
fn zone_row(
    id: &str,
    label: &str,
    zone: &str,
    stamp: i64,
    score: i64,
    hero: bool,
    origin: &str,
    board: &Board,
) -> Option<Value> {
    let tz = TimeZone::get(zone).ok()?;
    let ts = Timestamp::from_second(stamp).ok()?;
    let zt = ts.to_zoned(tz);

    let clock = zt.strftime("%H:%M").to_string();
    let pretty = zt.strftime("%a %-d %b").to_string();
    let mut abbr = zt.strftime("%Z").to_string();
    let local_tz = TimeZone::get(&board.local).unwrap_or(TimeZone::UTC);
    let zl = ts.to_zoned(local_tz);

    // The day is shown whenever it differs from yours — the thing people
    // get wrong, and a bare "09:00" hides it completely.
    let day_diff = day_num(zt.date()) - day_num(zl.date());
    let (day_word, day_phrase) = match day_diff {
        0 => (String::new(), String::new()),
        1 => ("Tomorrow".to_string(), "already tomorrow".to_string()),
        -1 => ("Yesterday".to_string(), "still yesterday".to_string()),
        d if d > 0 => (format!("+{d} days"), format!("{d} days ahead of you")),
        d => (format!("{d} days"), format!("{} days behind you", -d)),
    };

    // "UTC · UTC" says nothing twice. The abbreviation earns its place only
    // when it is not just the zone name again.
    let zone_tail = zone.rsplit('/').next().unwrap_or(zone);
    if abbr == zone_tail {
        abbr = String::new();
    }

    let shift = if zone == board.local {
        "Local".to_string()
    } else {
        let here = offset_minutes(&board.local, stamp)?;
        let there = offset_minutes(zone, stamp)?;
        describe_shift(there - here)
    };

    let (title, subtitle, detail, accessory, view) = if hero {
        let title = clock.clone();
        let mut subtitle = zt.strftime("%A %-d %B").to_string();
        if !day_phrase.is_empty() {
            subtitle = format!("{subtitle}  ·  {day_phrase}");
        }
        let mut detail = if label != zone {
            format!("{label}  ·  {zone}")
        } else {
            zone.to_string()
        };
        // Etc/GMT-5 next to "UTC+5" reads as a contradiction, so the offset
        // zones show only the name that means what it says.
        if zone.starts_with("Etc/GMT") {
            detail = label.to_string();
        }
        if !abbr.is_empty() {
            detail = format!("{detail} ({abbr})");
        }
        detail = format!("{detail}  ·  {shift}");
        if !origin.is_empty() {
            detail = format!("{detail}  ·  from {origin}");
        }
        (title, subtitle, detail, String::new(), "hero")
    } else {
        // `zones` is a layout, not a list of sentences, so the parts go over
        // separately: joining them here and splitting them there is how a
        // view ends up unable to give the time more weight than the zone id.
        (
            label.to_string(),
            shift,
            if abbr.is_empty() {
                zone.to_string()
            } else {
                abbr
            },
            day_word,
            "zones",
        )
    };

    let copy = format!("printf %s {} | wl-copy", shq(&clock));
    let full = format!(
        "printf %s {} | wl-copy",
        shq(&zt.strftime("%Y-%m-%d %H:%M %Z").to_string())
    );
    let unix = format!("printf %s {} | wl-copy", shq(&stamp.to_string()));
    let mut actions = vec![
        json!({"title": "Copy Time", "shortcut": "↵", "exec": copy}),
        json!({"title": "Copy Date and Time", "exec": full}),
    ];
    actions.extend(discord_actions(stamp));
    actions.push(json!({"title": "Copy Unix Seconds", "exec": unix}));

    Some(json!({
        "id": id,
        "title": title,
        "subtitle": subtitle,
        "detail": detail,
        "accessory": accessory,
        "clock": clock,
        "dayline": pretty,
        "zoneid": zone,
        "exec": copy,
        "score": score,
        "view": view,
        "glyph": "",
        "actions": actions,
    }))
}

/// Days since the epoch for a civil `Date` — the script's two `date -d`
/// calls and their `/86400`, except a civil difference is exact even across
/// a 23- or 25-hour day.
fn day_num(d: jiff::civil::Date) -> i64 {
    days::days_from_civil(
        i64::from(d.year()),
        i64::from(d.month()),
        i64::from(d.day()),
    )
}

/// The column: your own zone first, then everything configured, each zone
/// once.
pub(super) fn column(stamp: i64, mut score: i64, skip: &str, board: &Board) -> Vec<Value> {
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    let own = (
        names::label_for_zone(&board.local, board),
        board.local.clone(),
    );
    for (label, zone) in std::iter::once(own).chain(board.pairs.iter().cloned()) {
        // A repeated zone does not earn a second row, but it does count as
        // seen: the same name twice later is still a repeat, and the hero's
        // zone is the one place the column is already showing.
        if !seen.insert(zone.clone()) || zone == skip {
            continue;
        }
        if let Some(row) = zone_row(
            &format!("z-{zone}"),
            &label,
            &zone,
            stamp,
            score,
            false,
            "",
            board,
        ) {
            rows.push(row);
        }
        score -= 100;
    }
    rows
}

/// The row that says what could not be read — a real answer, not silence:
/// `tz:` is typed on purpose and "nothing happened" is the worst failure it
/// can have.
pub(super) fn fail_row(title: &str, detail: &str) -> Vec<Value> {
    vec![json!({
        "id": "tz-none",
        "title": title,
        "subtitle": "Timezone",
        "detail": detail,
        "exec": "",
        "score": 90000,
        "view": "list",
        "glyph": "",
    })]
}

/// The hero row plus the column underneath it — the single conversion that
/// is the whole question, and what it is everywhere else.
pub(super) fn hero_and_column(
    dst_zone: &str,
    stamp: i64,
    origin: &str,
    board: &Board,
) -> Vec<Value> {
    let label = names::label_for_zone(dst_zone, board);
    let mut rows = Vec::new();
    if let Some(hero) = zone_row(
        "tz-hero", &label, dst_zone, stamp, 99000, true, origin, board,
    ) {
        rows.push(hero);
    }
    rows.extend(column(stamp, 94000, dst_zone, board));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> Board {
        Board::load(&Settings::merge(&json!({})))
    }

    #[test]
    fn the_shift_is_words_not_numbers() {
        assert_eq!(describe_shift(0), "same time");
        assert_eq!(describe_shift(60), "1h ahead");
        assert_eq!(describe_shift(-60), "1h behind");
        assert_eq!(describe_shift(330), "5h30 ahead");
        assert_eq!(describe_shift(-570), "9h30 behind");
        assert_eq!(describe_shift(30), "30m ahead");
    }

    #[test]
    fn a_zone_row_is_the_scripts_shape() {
        let b = board();
        // 1735689600 = 2025-01-01T00:00:00Z
        let row = zone_row(
            "tz-hero",
            "Tokyo",
            "Asia/Tokyo",
            1735689600,
            99000,
            true,
            "",
            &b,
        )
        .expect("tokyo resolves");
        assert_eq!(row["clock"], "09:00");
        assert_eq!(row["title"], "09:00");
        assert_eq!(row["zoneid"], "Asia/Tokyo");
        assert_eq!(row["view"], "hero");
        assert_eq!(row["dayline"], "Wed 1 Jan");
        assert_eq!(row["actions"].as_array().unwrap().len(), 12);
        // `printf '%q'` escapes rather than quotes: "2025-01-01 09:00 JST"
        // goes over as backslash-space, not '…'.
        assert_eq!(
            row["actions"][1]["exec"],
            "printf %s 2025-01-01\\ 09:00\\ JST | wl-copy"
        );
        assert_eq!(
            row["actions"][2]["exec"],
            "printf %s '<t:1735689600:F>' | wl-copy"
        );
    }

    #[test]
    fn the_column_puts_home_first_and_never_repeats() {
        let s = Settings::merge(&json!({
            "timezones": [{"label": "Home", "zone": "UTC"},
                          {"label": "Home again", "zone": "UTC"},
                          {"label": "Ana", "zone": "Europe/Lisbon"}]
        }));
        let b = Board::load(&s);
        let rows = column(1735689600, 95000, "", &b);
        let zoneids: Vec<&str> = rows.iter().map(|r| r["zoneid"].as_str().unwrap()).collect();
        // Your own zone leads; UTC appears at most once even though the file
        // named it twice; written order holds for what remains.
        assert_eq!(zoneids[0], b.local.as_str());
        assert_eq!(zoneids.iter().filter(|z| **z == "UTC").count(), 1);
        assert_eq!(zoneids.last(), Some(&"Europe/Lisbon"));
    }

    #[test]
    fn the_column_skips_the_hero_zone() {
        let b = board();
        let rows = column(1735689600, 94000, "Asia/Tokyo", &b);
        assert!(rows.iter().all(|r| r["zoneid"] != "Asia/Tokyo"));
        assert!(rows.iter().any(|r| r["zoneid"] == "America/New_York"));
    }
}
