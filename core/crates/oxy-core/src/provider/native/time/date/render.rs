use serde_json::{Value, json};

use crate::provider::native::time::days;
use crate::support::quote::quote;

use super::parse::plural;

// ---------------------------------------------------------------- phrasing

/// Years, months and days between two dates, as words. 128 days is a number;
/// "4 months and 6 days" is an answer, and the number is still on the detail
/// line for whoever came for the number.
///
/// The months are counted by the calendar rather than divided out of the
/// days: February to March is one month whatever the day count says. `+N
/// months` from the 31st lands in the month after next, so the anchor is
/// walked back until it fits.
fn span_words(from: i64, to: i64) -> String {
    let (fy, fm, fd) = days::civil_from_days(from);
    let (ty, tm, td) = days::civil_from_days(to);
    let mut months = (ty - fy) * 12 + (tm - fm);
    if td < fd {
        months -= 1;
    }
    if months < 0 {
        months = 0;
    }
    let mut anchor = days::add_months_daynum(from, months);
    while months > 0 && anchor > to {
        months -= 1;
        anchor = days::add_months_daynum(from, months);
    }
    let rest = to - anchor;

    let years = months / 12;
    let months = months % 12;
    let mut parts: Vec<String> = Vec::new();
    if years > 0 {
        parts.push(plural(years, "year"));
    }
    if months > 0 {
        parts.push(plural(months, "month"));
    }
    if rest > 0 {
        parts.push(plural(rest, "day"));
    }
    match parts.len() {
        0 => "0 days".to_string(),
        1 => parts[0].clone(),
        2 => format!("{} and {}", parts[0], parts[1]),
        _ => format!("{}, {} and {}", parts[0], parts[1], parts[2]),
    }
}

/// How far away, in the words a person would use. Under a month a day count
/// is the clearest thing there is; past that it stops meaning much and the
/// calendar does the talking.
fn distance_words(iso: i64, today: i64) -> String {
    let days = iso - today;
    match days {
        0 => return "today".to_string(),
        1 => return "tomorrow".to_string(),
        -1 => return "yesterday".to_string(),
        _ => {}
    }
    if days > 0 {
        if days <= 30 {
            format!("in {}", plural(days, "day"))
        } else {
            format!("in {}", span_words(today, iso))
        }
    } else if days >= -30 {
        format!("{} ago", plural(-days, "day"))
    } else {
        format!("{} ago", span_words(iso, today))
    }
}

/// Monday to Friday across a half-open span, so the weekday count and the day
/// count above it are counting the same days.
fn weekdays_between(a: i64, b: i64) -> i64 {
    let (a, b) = if a > b { (b, a) } else { (a, b) };
    let n = b - a;
    let mut out = (n / 7) * 5;
    let dow = days::weekday_u(a);
    for i in 0..(n % 7) {
        if ((dow - 1 + i) % 7) < 5 {
            out += 1;
        }
    }
    out
}

/// Two dates as one phrase, with the year said once when once is enough.
/// "29 December to 4 January 2026" would be a lie about the December: the
/// week runs from 2025 and the row has to say so.
fn span_text(from: i64, to: i64) -> String {
    let (fy, fm, _) = days::civil_from_days(from);
    let (ty, tm, _) = days::civil_from_days(to);
    if fy != ty {
        format!("{} to {}", days::long_date(from), days::long_date(to))
    } else if fm == tm {
        let (_, _, fd) = days::civil_from_days(from);
        format!("{fd} to {}", days::long_date(to))
    } else {
        format!("{} to {}", days::day_month(from), days::long_date(to))
    }
}

/// Where a stretch of days sits relative to now, as a sentence.
fn period_phrase(from: i64, to: i64, word: &str, today: i64) -> String {
    if today >= from && today <= to {
        return format!("the {word} we are in");
    }
    let phrase = distance_words(from, today);
    if phrase.starts_with("in ") || phrase == "tomorrow" {
        format!("starting {phrase}")
    } else {
        format!("started {phrase}")
    }
}

// ------------------------------------------------------------------- rows

/// The answer a resolution produced, before it is drawn: a day, a stretch of
/// days, or the gap between two.
pub(super) enum Spec {
    /// One date — a holiday, a resolved phrase, an edge of a period.
    Day {
        day: i64,
        label: String,
        lead: bool,
        note: Option<String>,
    },
    /// A stretch of days with a name: a week, a quarter, a month, a weekend.
    Period {
        id: String,
        from: i64,
        to: i64,
        name: String,
        word: &'static str,
        action: &'static str,
        detail: String,
    },
    /// "from 1 jan to today" — the gap itself is the answer.
    Range { from: i64, to: i64 },
}

/// One date, said as a sentence. The date leads because that is the question
/// in every one of these forms, the sentence under it is why the row is on
/// screen, and the counts come last for whoever came for a count.
fn date_row(iso: i64, label: &str, lead: bool, note: Option<&str>, today: i64) -> Value {
    let full = days::full_date(iso);
    let weekday = days::WEEKDAYS[days::weekday_of(iso) as usize];
    let week = {
        let (y, m, d) = days::civil_from_days(iso);
        days::iso_week(y, m, d)
    };
    let (y, _, _) = days::civil_from_days(iso);
    let doy = days::day_of_year(iso);
    let total = days::day_of_year(days::days_from_civil(y, 12, 31));
    let days = (iso - today).abs();

    let distance = distance_words(iso, today);
    let subtitle = if label.is_empty() {
        let mut c = distance.chars();
        match c.next() {
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            None => distance,
        }
    } else {
        format!("{label}, {distance}")
    };
    let copy = format!("printf %s {} | wl-copy", quote(&days::iso_date(iso)));

    // The day count earns its place on the detail line only when the sentence
    // above it has stopped counting days.
    let mut counts = format!("week {week}  ·  day {doy} of {total}");
    if days > 30 {
        counts = format!("{}  ·  {counts}", plural(days, "day"));
    }
    // A reading that was a choice says so, ahead of the counts, because the
    // counts are only right if the reading was.
    if let Some(note) = note
        && !note.is_empty()
    {
        counts = format!("{note}  ·  {counts}");
    }

    if lead {
        json!({
            "id": format!("{}-{}", days::iso_date(iso), if label.is_empty() { "date" } else { label }),
            "title": full,
            "subtitle": subtitle,
            "detail": counts,
            "view": "hero",
            "score": 99000,
            "exec": copy,
            "actions": [
                { "title": "Copy ISO Date", "shortcut": "↵", "exec": copy },
                { "title": "Copy Full Date", "exec": format!("printf %s {} | wl-copy", quote(&full)) },
            ]
        })
    } else {
        let title = if label.is_empty() { weekday } else { label };
        json!({
            "id": format!("{}-{}", days::iso_date(iso), if label.is_empty() { "date" } else { label }),
            "title": format!("{title}, {}", days::long_date(iso)),
            "subtitle": distance_words(iso, today),
            "exec": copy,
        })
    }
}

/// A stretch of days: a week, a quarter, a month, a weekend. The days are the
/// answer, so the days are the title, and the name of the stretch goes under
/// it.
#[allow(clippy::too_many_arguments)]
fn period_row(
    id: &str,
    from: i64,
    to: i64,
    name: &str,
    word: &str,
    action: &str,
    detail: &str,
    today: i64,
) -> Value {
    let days_n = to - from + 1;
    let wd = weekdays_between(from, to + 1);
    let mut detail = if detail.is_empty() {
        String::new()
    } else {
        format!("{detail}  ·  ")
    };
    detail.push_str(&plural(days_n, "day"));
    // A weekend has no weekdays in it, and saying "0 weekdays" under one is
    // the field dump this row exists to avoid.
    if wd > 0 {
        detail.push_str(&format!("  ·  {} weekdays", wd));
    }
    let v = format!("{} to {}", days::iso_date(from), days::iso_date(to));
    json!({
        "id": id,
        "title": span_text(from, to),
        "subtitle": format!("{name}, {}", period_phrase(from, to, word, today)),
        "detail": detail,
        "view": "hero",
        "score": 99000,
        "exec": format!("printf %s {} | wl-copy", quote(&v)),
        "actions": [
            { "title": action, "shortcut": "↵",
              "exec": format!("printf %s {} | wl-copy", quote(&v)) }
        ]
    })
}

pub(super) fn render(spec: &Spec, today: i64) -> Value {
    match spec {
        Spec::Day {
            day,
            label,
            lead,
            note,
        } => date_row(*day, label, *lead, note.as_deref(), today),
        Spec::Period {
            id,
            from,
            to,
            name,
            word,
            action,
            detail,
        } => period_row(id, *from, *to, name, word, action, detail, today),
        Spec::Range { from, to } => {
            let (a, b) = (*from.min(to), *from.max(to));
            let days_n = b - a;
            let weeks = days_n / 7;
            let rest = days_n % 7;
            let wd = weekdays_between(*from, *to);
            let (from_iso, to_iso) = (days::iso_date(*from), days::iso_date(*to));
            let span = if from <= to {
                span_words(*from, *to)
            } else {
                span_words(*to, *from)
            };
            let (fy, _, _) = days::civil_from_days(*from);
            let (ty, _, _) = days::civil_from_days(*to);
            let from_text = if fy == ty {
                days::day_month(*from)
            } else {
                days::long_date(*from)
            };
            let detail = if weeks > 0 {
                if rest > 0 {
                    format!(
                        "{} and {}  ·  {} weekdays",
                        plural(weeks, "week"),
                        plural(rest, "day"),
                        wd
                    )
                } else {
                    format!("{}  ·  {} weekdays", plural(weeks, "week"), wd)
                }
            } else {
                format!("{wd} weekdays")
            };
            let v = days_n.to_string();
            json!({
                "id": format!("range-{from_iso}-{to_iso}"),
                "title": plural(days_n, "day"),
                "subtitle": format!("{span}, from {from_text} to {}", days::long_date(*to)),
                "detail": detail,
                "view": "hero",
                "score": 99000,
                "exec": format!("printf %s {} | wl-copy", quote(&v)),
                "actions": [
                    { "title": "Copy Day Count", "shortcut": "↵",
                      "exec": format!("printf %s {} | wl-copy", quote(&v)) }
                ]
            })
        }
    }
}
