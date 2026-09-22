use std::sync::OnceLock;

use crate::provider::native::time::days;

use super::holidays::{Prefer, match_holidays};
use super::words::{normalize_relative, normalize_words};

// --------------------------------------------------------------- resolving

/// d/m/y or m/d/y? A number above twelve settles it, and when both halves
/// could be a month the locale does: `d_fmt` for this machine says which way
/// round it writes them, so a Brazilian desktop reads 25/12/2026 as Christmas
/// rather than as nothing at all.
fn day_first_locale() -> bool {
    static DAY_FIRST: OnceLock<bool> = OnceLock::new();
    *DAY_FIRST.get_or_init(|| {
        // `locale d_fmt` answers the same question the script asked `%x` of a
        // known date, without formatting one. Missing locale → month-first,
        // the same fallback the script's pattern-match miss produced. `probe`
        // because a OnceLock init cannot await — and cannot hang either.
        crate::provider::process::probe(&["locale", "d_fmt"], std::time::Duration::from_secs(2))
            .and_then(|o| {
                let fmt = o.trim().to_string();
                let d = fmt.find("%d").or_else(|| fmt.find("%e"));
                let m = fmt.find("%m");
                match (d, m) {
                    (Some(d), Some(m)) => Some(d < m),
                    _ => None,
                }
            })
            .unwrap_or(false)
    })
}

fn day_first(a: i64, b: i64) -> bool {
    if a > 12 {
        return true;
    }
    if b > 12 {
        return false;
    }
    day_first_locale()
}

/// GNU's two-digit year pivot: 00–68 is this century, 69–99 the last.
fn pivot_year(y: i64) -> i64 {
    if y < 100 {
        if y <= 68 { 2000 + y } else { 1900 + y }
    } else {
        y
    }
}

/// `a/b/c` or `a-b-c` or `a.b.c` where two parts are 1–2 digits and the year
/// is last, day/month order settled by [`day_first`]. None when it is not a
/// date at all.
fn slash_date(a: i64, b: i64, c: i64) -> Option<i64> {
    let (d, m) = if day_first(a, b) { (a, b) } else { (b, a) };
    let y = pivot_year(c);
    days::valid_ymd(y, m, d).then(|| days::days_from_civil(y, m, d))
}

/// What a slashed date would have meant the other way round, if the other way
/// round is still on the table.
pub(super) fn slash_note(t: &str) -> Option<String> {
    let t = normalize_relative(&normalize_words(&t.to_lowercase()));
    let (a, b, c) = split_three(&t)?;
    if a > 12 || b > 12 || a == b {
        return None;
    }
    let y = pivot_year(c);
    let other = if day_first(a, b) {
        days::valid_ymd(y, a, b).then(|| days::days_from_civil(y, a, b))
    } else {
        days::valid_ymd(y, b, a).then(|| days::days_from_civil(y, b, a))
    }?;
    let read = if day_first(a, b) {
        "day/month"
    } else {
        "month/day"
    };
    Some(format!(
        "read as {read}  ·  {b}/{a} is {}",
        days::day_month(other)
    ))
}

/// `^([0-9]{1,2})[/.-]([0-9]{1,2})[/.-]([0-9]{2,4})$` — the three numbers of a
/// slashed date, separators mixed however they were typed.
fn split_three(t: &str) -> Option<(i64, i64, i64)> {
    let parts: Vec<&str> = t.split(['/', '.', '-']).collect();
    if parts.len() != 3 {
        return None;
    }
    let [a, b, c] = [parts[0], parts[1], parts[2]];
    if !(1..=2).contains(&a.len())
        || !(1..=2).contains(&b.len())
        || !(2..=4).contains(&c.len())
        || ![a, b, c]
            .iter()
            .all(|s| s.chars().all(|ch| ch.is_ascii_digit()))
    {
        return None;
    }
    Some((a.parse().ok()?, b.parse().ok()?, c.parse().ok()?))
}

const MONTH_ABBRS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
pub(super) const DAY_NAMES: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

/// The narrow gate that keeps an unscoped answer quiet unless it is confident.
fn looks_like_date(t: &str) -> bool {
    // YYYY-M-D
    if iso_parts(t).is_some() {
        return true;
    }
    // D/M/Y(YY)
    if split_three(t).is_some() {
        return true;
    }
    // a month name and a digit
    if MONTH_ABBRS.iter().any(|m| t.contains(m)) && t.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    if matches!(t, "today" | "now" | "tonight" | "tomorrow" | "yesterday") {
        return true;
    }
    if DAY_NAMES.contains(&t) {
        return true;
    }
    if let Some(rest) = t
        .strip_prefix("next ")
        .or_else(|| t.strip_prefix("last "))
        .or_else(|| t.strip_prefix("this "))
        && (matches!(rest, "week" | "month" | "year" | "fortnight") || DAY_NAMES.contains(&rest))
    {
        return true;
    }
    // [+-]?N (minute|min|hour|hr|day|week|month|year|fortnight)s? [ago]
    if rel_parts(t).is_some() {
        return true;
    }
    false
}

/// `YYYY-M-D` — the one numeric form the gate admits with dashes:
/// `^[0-9]{4}-[0-9]{1,2}-[0-9]{1,2}$`, valid dates only.
fn iso_parts(t: &str) -> Option<(i64, i64, i64)> {
    let parts: Vec<&str> = t.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let [ys, ms, ds] = [parts[0], parts[1], parts[2]];
    if ys.len() != 4
        || !(1..=2).contains(&ms.len())
        || !(1..=2).contains(&ds.len())
        || ![ys, ms, ds]
            .iter()
            .all(|s| s.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let (y, m, d) = (ys.parse().ok()?, ms.parse().ok()?, ds.parse().ok()?);
    days::valid_ymd(y, m, d).then_some((y, m, d))
}

/// `[+-]?N unit(s) [ago]` — the pieces of a relative offset.
fn rel_parts(t: &str) -> Option<(i64, &'static str, bool)> {
    let (t, ago) = match t.strip_suffix(" ago") {
        Some(r) => (r, true),
        None => (t, false),
    };
    let mut it = t.split_whitespace();
    let n: i64 = it.next()?.parse().ok()?;
    let unit = it.next()?;
    if it.next().is_some() {
        return None;
    }
    let unit = match unit.trim_end_matches('s') {
        "minute" | "min" => "min",
        "hour" | "hr" => "hr",
        "day" => "day",
        "week" => "week",
        "fortnight" => "fortnight",
        "month" => "month",
        "year" => "year",
        _ => return None,
    };
    let n = if ago { -n } else { n };
    Some((n, unit, ago))
}

/// A month-name word: a full name, or a prefix of one from three letters.
/// "sept" is already rewritten by `normalize_words`; "novx" is nothing.
pub(super) fn month_of_word(w: &str) -> Option<i64> {
    if w.len() < 3 {
        return None;
    }
    for (i, name) in MONTH_ABBRS.iter().enumerate() {
        let full = days::MONTHS[i].to_lowercase();
        if *name == w || full == w || (w.len() >= 3 && full.starts_with(w)) {
            return Some(i as i64 + 1);
        }
    }
    None
}

/// The slice of `date -d` the gate can actually reach: named days, relative
/// offsets, weekday names, and month-name-plus-number forms. Anything else is
/// not a question this provider answers.
fn gnu_day(t: &str, today: i64, today_year: i64) -> Option<i64> {
    if let Some((y, m, d)) = iso_parts(t) {
        return Some(days::days_from_civil(y, m, d));
    }
    match t {
        "today" | "now" | "tonight" => return Some(today),
        "tomorrow" => return Some(today + 1),
        "yesterday" => return Some(today - 1),
        _ => {}
    }
    // Bare weekday: the coming one, today counting.
    if let Some(w) = DAY_NAMES.iter().position(|d| *d == t) {
        let want = (w as i64 + 1) % 7; // DAY_NAMES is Monday-first; weekday() is 0=Sun
        let delta = (want - days::weekday_of(today) + 7) % 7;
        return Some(today + delta);
    }
    // this|next|last <weekday|week|fortnight|month|year>
    for (word, dir) in [("this ", 0i64), ("next ", 1), ("last ", -1)] {
        if let Some(rest) = t.strip_prefix(word) {
            if let Some(w) = DAY_NAMES.iter().position(|d| *d == rest) {
                let want = (w as i64 + 1) % 7;
                let dow = days::weekday_of(today);
                let delta = match dir {
                    // this: the coming one, today counting — `this monday`
                    // on a Tuesday is six days out, not yesterday.
                    0 => (want - dow + 7) % 7,
                    // next: strictly after today — `next friday` on Tuesday
                    // is this Friday, but `next tuesday` is a week out.
                    1 => match (want - dow + 7) % 7 {
                        0 => 7,
                        d => d,
                    },
                    // last: the first one strictly before today
                    _ => {
                        let d = (dow - want + 7) % 7;
                        -if d == 0 { 7 } else { d }
                    }
                };
                return Some(today + delta);
            }
            let days_off = match rest {
                "week" => Some(7),
                "fortnight" => Some(14),
                _ => None,
            };
            if let Some(d) = days_off {
                return Some(today + dir * d);
            }
            if rest == "month" {
                return Some(days::add_months_daynum(today, dir));
            }
            if rest == "year" {
                // GNU overflows the day rather than refusing it — 29 February
                // next year is 1 March, the same walk months do.
                return Some(days::add_months_daynum(today, dir * 12));
            }
            return None;
        }
    }
    // [+-]?N unit(s) [ago]
    if let Some((n, unit, _ago)) = rel_parts(t) {
        // Sub-day offsets run on the *local* wall clock like `date -d` did —
        // "2 hours" near midnight is tomorrow. A UTC day boundary lands the
        // answer a day off inside the morning/evening windows.
        let sub_day = |secs: i64| -> Option<i64> {
            let z = jiff::Timestamp::now()
                .checked_add(jiff::Span::new().seconds(secs))
                .ok()?
                .to_zoned(jiff::tz::TimeZone::system());
            let d = z.date();
            Some(days::days_from_civil(
                i64::from(d.year()),
                i64::from(d.month()),
                i64::from(d.day()),
            ))
        };
        return Some(match unit {
            "min" => sub_day(n * 60)?,
            "hr" => sub_day(n * 3600)?,
            "day" => today + n,
            "week" => today + n * 7,
            "fortnight" => today + n * 14,
            "month" => days::add_months_daynum(today, n),
            "year" => days::add_months_daynum(today, n * 12),
            _ => return None,
        });
    }
    // Month-name forms: "27 november 2027", "november 27", "nov 27 2027".
    // The gate already promised a month and a digit live in the text.
    let mut month = None;
    let mut day = None;
    let mut year = None;
    for w in t.split([' ', ',']) {
        let w = w.trim_matches('.');
        if w.is_empty() {
            continue;
        }
        if month.is_none()
            && let Some(m) = month_of_word(w)
        {
            month = Some(m);
            continue;
        }
        if let Ok(n) = w.parse::<i64>() {
            if (1..=31).contains(&n) && day.is_none() {
                day = Some(n);
            } else if n.to_string().len() >= 2 && year.is_none() {
                year = Some(pivot_year(n));
            }
        }
    }
    if let (Some(m), Some(d)) = (month, day) {
        let y = year.unwrap_or(today_year);
        if days::valid_ymd(y, m, d) {
            return Some(days::days_from_civil(y, m, d));
        }
    }
    None
}

/// One piece of text to one day number, or nothing. Named days go first,
/// because the strict grammar reads "easter" as nothing at all and would take
/// the query anyway.
pub(super) fn resolve_one(text: &str, prefer: Prefer, today: i64, today_year: i64) -> Option<i64> {
    // A bare three-letter weekday is refused on purpose. `date:` answers
    // unscoped, so every query of three characters passes through here, and
    // "sat", "sun", "mon" and "wed" are the openings of far more searches
    // than they are questions about a day. Four letters and anything
    // qualified do answer.
    if matches!(text, "mon" | "tue" | "wed" | "thu" | "fri" | "sat" | "sun") {
        return None;
    }
    let t = normalize_relative(text);

    // A trailing year belongs to the name in front of it: "christmas 2028".
    if let Some((name, y)) = t.rsplit_once(' ')
        && y.len() == 4
        && y.chars().all(|c| c.is_ascii_digit())
        && let Ok(year) = y.parse::<i64>()
        && name.chars().any(|c| !c.is_ascii_digit())
    {
        let hits = match_holidays(name, year, true, prefer, today, today_year);
        if let Some((_, day, _)) = hits.first() {
            return Some(*day);
        }
    }
    let hits = match_holidays(&t, today_year, false, prefer, today, today_year);
    if let Some((_, day, _)) = hits.first() {
        return Some(*day);
    }

    let t = normalize_relative(&normalize_words(&t));

    if let Some((a, b, c)) = split_three(&t)
        && (1..=31).contains(&a)
        && (1..=31).contains(&b)
    {
        if let Some(d) = slash_date(a, b, c) {
            return Some(d);
        }
        return None;
    }

    // 20261225. Eight digits are a date only if they are one: a bare number
    // that happens to be eight long is far more often an id.
    if t.len() == 8
        && t.chars().all(|c| c.is_ascii_digit())
        && let (Ok(y), Ok(m), Ok(d)) = (
            t[0..4].parse::<i64>(),
            t[4..6].parse::<i64>(),
            t[6..8].parse::<i64>(),
        )
        && days::valid_ymd(y, m, d)
    {
        return Some(days::days_from_civil(y, m, d));
    }

    if !looks_like_date(&t) {
        return None;
    }
    gnu_day(&t, today, today_year)
}

pub(super) fn plural(n: i64, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::native::time::days;

    /// 2026-09-22 is a Tuesday — the anchor every `date -d` check below was
    /// run against on GNU coreutils.
    fn tue() -> i64 {
        days::days_from_civil(2026, 9, 22)
    }

    #[test]
    fn weekday_modifiers_match_gnu() {
        let t = tue();
        let fri = days::days_from_civil(2026, 9, 25);
        let mon = days::days_from_civil(2026, 9, 28);
        let tue_next = days::days_from_civil(2026, 9, 29);
        let fri_last = days::days_from_civil(2026, 9, 18);
        // `this`/`next` never look backwards or add a spare week; `next`
        // differs from `this` only when the named day is today itself.
        assert_eq!(gnu_day("this friday", t, 2026), Some(fri));
        assert_eq!(gnu_day("next friday", t, 2026), Some(fri));
        assert_eq!(gnu_day("this monday", t, 2026), Some(mon));
        assert_eq!(gnu_day("next monday", t, 2026), Some(mon));
        assert_eq!(gnu_day("this tuesday", t, 2026), Some(t));
        assert_eq!(gnu_day("next tuesday", t, 2026), Some(tue_next));
        assert_eq!(gnu_day("last friday", t, 2026), Some(fri_last));
        assert_eq!(gnu_day("last tuesday", t, 2026), Some(t - 7));
    }

    #[test]
    fn unit_modifiers_match_gnu() {
        let t = tue();
        // `date -d "this month"` is today, not next month — `this` is the
        // zero ordinal on the unit arms.
        assert_eq!(gnu_day("this week", t, 2026), Some(t));
        assert_eq!(gnu_day("next week", t, 2026), Some(t + 7));
        assert_eq!(gnu_day("last fortnight", t, 2026), Some(t - 14));
        assert_eq!(
            gnu_day("next month", t, 2026),
            Some(days::add_months_daynum(t, 1))
        );
    }
}
