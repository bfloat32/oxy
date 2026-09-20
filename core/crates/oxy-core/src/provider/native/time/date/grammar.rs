use crate::provider::native::time::days;

use super::holidays::{Prefer, monday_of, quarter_start};
use super::parse::{DAY_NAMES, month_of_word, resolve_one};
use super::render::Spec;
use super::words::normalize_words;

// ------------------------------------------------------------------ ranges

/// "from 1 jan to today", "1 jan to christmas", "days until christmas". The
/// gap is the answer, so the gap is the title. Both ends have to resolve or
/// this was never a range, and the single-date path gets its turn.
pub(super) fn try_range(text: &str, today: i64, today_year: i64) -> Option<Vec<Spec>> {
    let mut ends: Option<(Option<i64>, Option<i64>)> = None;
    'outer: {
        // "days until christmas", "how long to go until friday". The left end
        // is always today; the right is a date said however it comes.
        for lead in [
            "how many days ",
            "how many weeks ",
            "how long ",
            "countdown ",
            "days ",
            "weeks ",
            "time ",
        ] {
            if let Some(rest) = text.strip_prefix(lead) {
                for mid in ["to go until ", "until ", "till ", "to "] {
                    if let Some(right) = rest.strip_prefix(mid) {
                        ends = Some((
                            Some(today),
                            resolve_one(right, Prefer::Next, today, today_year),
                        ));
                        break 'outer;
                    }
                }
            }
        }
        // "days since new year" reaches backwards, and the name in it has to
        // reach backwards too.
        for lead in ["how many days ", "how long ", "days ", "weeks ", "time "] {
            if let Some(rest) = text.strip_prefix(lead)
                && let Some(right) = rest.strip_prefix("since ")
            {
                ends = Some((
                    resolve_one(right, Prefer::Past, today, today_year),
                    Some(today),
                ));
                break 'outer;
            }
        }
        // "between x and y". The left of the regex is greedy, so the split is
        // at the last " and ".
        for lead in ["how many days between ", "between "] {
            if let Some(rest) = text.strip_prefix(lead)
                && let Some((l, r)) = rest.rsplit_once(" and ")
            {
                ends = Some((
                    resolve_one(l, Prefer::Next, today, today_year),
                    resolve_one(r, Prefer::Next, today, today_year),
                ));
                break 'outer;
            }
        }
        // "from 1 jan to today", "x until y" — the mid splits at its last
        // position, because the left of the regex is greedy too.
        let body = text.strip_prefix("from ").unwrap_or(text);
        let mut best: Option<(usize, usize)> = None;
        for mid in [" to ", " until ", " till ", " thru ", " through "] {
            if let Some(pos) = body.rfind(mid)
                && best.is_none_or(|(p, _)| pos > p)
            {
                best = Some((pos, mid.len()));
            }
        }
        if let Some((pos, len)) = best {
            ends = Some((
                resolve_one(&body[..pos], Prefer::Next, today, today_year),
                resolve_one(&body[pos + len..], Prefer::Next, today, today_year),
            ));
            break 'outer;
        }
        // "1 jan - 31 dec" is the same question with a dash in it. Both
        // spaces are required so "in -5 days" is still a day, not a range.
        let mut best: Option<(usize, usize)> = None;
        for mid in [" - ", " -- ", " – ", " — ", " .. ", " ... ", " > ", " -> "] {
            if let Some(pos) = body.rfind(mid)
                && best.is_none_or(|(p, _)| pos > p)
            {
                best = Some((pos, mid.len()));
            }
        }
        if let Some((pos, len)) = best {
            ends = Some((
                resolve_one(body[..pos].trim_end(), Prefer::Next, today, today_year),
                resolve_one(
                    body[pos + len..].trim_start(),
                    Prefer::Next,
                    today,
                    today_year,
                ),
            ));
        }
    }
    let (Some(l), Some(r)) = ends? else {
        return None;
    };
    Some(vec![Spec::Range { from: l, to: r }])
}

// ------------------------------------------------------------------- weeks

/// "week 34", "week 34 2027", "week". ISO weeks, so a week starts on Monday
/// and week 1 is the one holding 4 January.
pub(super) fn try_week(text: &str, today: i64, _today_year: i64) -> Option<Vec<Spec>> {
    // "w34", "wk 34" and "week 34 of 2027" are the same question written
    // shorter and longer. All three were silent.
    let t = 'norm: {
        for prefix in ["week", "wk", "w"] {
            if let Some(rest) = text.strip_prefix(prefix) {
                let rest = rest.trim();
                if rest.is_empty() {
                    break 'norm if prefix == "week" {
                        "week".to_string()
                    } else {
                        // "w" and "wk" alone match nothing the script knew.
                        return None;
                    };
                }
                let mut it = rest.split_whitespace();
                let n = it.next()?;
                if !n.chars().all(|c| c.is_ascii_digit()) || n.len() > 2 {
                    break 'norm text.to_string();
                }
                let year = match it.collect::<Vec<_>>().as_slice() {
                    [] => None,
                    [y] | ["of", y] if y.len() == 4 => Some((*y).to_string()),
                    _ => break 'norm text.to_string(),
                };
                break 'norm match year {
                    Some(y) => format!("week {n} {y}"),
                    None => format!("week {n}"),
                };
            }
        }
        text.to_string()
    };

    let (n, year);
    let mut rest = t.as_str();
    let mut shift = 0i64;
    let mut shifted = false;
    for w in ["next ", "last ", "this "] {
        if let Some(r) = rest.strip_prefix(w) {
            shift = match w {
                "next " => 7,
                "last " => -7,
                _ => 0,
            };
            rest = r;
            shifted = true;
            break;
        }
    }
    if rest == "week" {
        // "next week" used to answer with a single day seven days out, which
        // is not what the words mean: a week is a range and this view can
        // say so.
        let moved = today + shift;
        let (y, m, d) = days::civil_from_days(moved);
        n = days::iso_week(y, m, d);
        year = days::iso_year(moved);
    } else if shifted {
        // "next week 34" is not a form the script took.
        return None;
    } else {
        let y = rest.strip_prefix("week ")?;
        let mut it = y.split_whitespace();
        n = it.next()?.parse().ok()?;
        year = match it.next() {
            Some(y) => y.parse().ok()?,
            None => days::iso_year(today),
        };
        if it.next().is_some() {
            return None;
        }
    }
    if !(1..=53).contains(&n) {
        return None;
    }

    let jan4 = days::days_from_civil(year, 1, 4);
    let dow = days::weekday_u(jan4);
    let mon = jan4 - (dow - 1) + (n - 1) * 7;
    let sun = mon + 6;

    Some(vec![Spec::Period {
        id: format!("week-{year}-{n}"),
        from: mon,
        to: sun,
        name: format!("Week {n} of {year}"),
        word: "week",
        action: "Copy Week Range",
        detail: format!(
            "{year}-W{n:02}  ·  days {} to {} of the year",
            days::day_of_year(mon),
            days::day_of_year(sun)
        ),
    }])
}

// --------------------------------------------------- edges, quarters, months

/// "end of the month", "start of next month", "last day of the year".
/// Somebody planning says these far more often than they say a date.
pub(super) fn try_bounds(text: &str, today: i64, today_year: i64) -> Option<Vec<Spec>> {
    // " of " and " the " go away wherever they sit — "start of the month" is
    // the question written out in full.
    let mut t = text.replace(" of ", " ").replace(" the ", " ");
    for (prefix, edge) in [
        ("first day", "start"),
        ("last day", "end"),
        ("beginning", "start"),
        ("finish", "end"),
    ] {
        if let Some(rest) = t.strip_prefix(prefix) {
            t = format!("{edge}{rest}");
            break;
        }
    }

    let (edge, rest) = t
        .strip_prefix("start ")
        .map(|r| ("start", r))
        .or_else(|| t.strip_prefix("end ").map(|r| ("end", r)))?;
    let (shift, unit) = if let Some(u) = rest.strip_prefix("next ") {
        (1i64, u)
    } else if let Some(u) = rest.strip_prefix("last ") {
        (-1i64, u)
    } else if let Some(u) = rest.strip_prefix("this ") {
        (0i64, u)
    } else {
        (0i64, rest)
    };

    let (first, last, name) = match unit {
        "week" => {
            let f = monday_of(today) + shift * 7;
            let (y, m, d) = days::civil_from_days(f);
            (f, f + 6, format!("Week {}", days::iso_week(y, m, d)))
        }
        "month" => {
            let (y, m, _) = days::civil_from_days(today);
            let (y, m) = days::add_months(y, m, shift);
            let f = days::days_from_civil(y, m, 1);
            (
                f,
                f + days::days_in_month(y, m) - 1,
                days::MONTHS[(m - 1) as usize].to_string(),
            )
        }
        "year" => {
            let y = today_year + shift;
            (
                days::days_from_civil(y, 1, 1),
                days::days_from_civil(y, 12, 31),
                y.to_string(),
            )
        }
        "quarter" => {
            let (_, m, _) = days::civil_from_days(today);
            let f = quarter_start(today_year, (m - 1) / 3 + 1 + shift);
            let (y, qm, _) = days::civil_from_days(f);
            let last = days::add_months_daynum(f, 3) - 1;
            (f, last, format!("Q{} {y}", (qm - 1) / 3 + 1))
        }
        _ => return None,
    };

    let (day, name) = if edge == "start" {
        (first, format!("Start of {name}"))
    } else {
        (last, format!("End of {name}"))
    };
    Some(vec![Spec::Day {
        day,
        label: name,
        lead: true,
        note: None,
    }])
}

/// "q3", "q3 2027", "3rd quarter", "this quarter". A quarter is a stretch of
/// days like a week is, so it answers like one.
pub(super) fn try_quarter(text: &str, today: i64, today_year: i64) -> Option<Vec<Spec>> {
    let t = normalize_words(text);
    let w: Vec<&str> = t.split_whitespace().collect();
    let qn = |s: &str| -> Option<i64> {
        (s.len() == 2 && s.starts_with('q') && matches!(s.as_bytes()[1], b'1'..=b'4'))
            .then(|| (s.as_bytes()[1] - b'0') as i64)
    };
    let year4 = |s: &str| {
        (s.len() == 4 && s.chars().all(|c| c.is_ascii_digit()))
            .then(|| s.parse::<i64>().ok())
            .flatten()
    };
    let (q, year) = match w.as_slice() {
        [s] => (qn(s)?, today_year),
        [s, ys] if qn(s).is_some() => (qn(s)?, year4(ys)?),
        [ys, s] if year4(ys).is_some() && qn(s).is_some() => (qn(s)?, year4(ys)?),
        [ns, "quarter"] if matches!(*ns, "1" | "2" | "3" | "4") => (ns.parse().ok()?, today_year),
        [ns, "quarter", ys] => match (*ns, year4(ys)) {
            ("1" | "2" | "3" | "4", Some(y)) => (ns.parse().ok()?, y),
            _ => return None,
        },
        [which, "quarter"] if matches!(*which, "this" | "next" | "last") => {
            let (_, m, _) = days::civil_from_days(today);
            let mut qq = (m - 1) / 3 + 1;
            match *which {
                "next" => qq += 1,
                "last" => qq -= 1,
                _ => {}
            }
            // quarter_start walks the year across the boundary either way.
            let first = quarter_start(today_year, qq);
            let (y, qm, _) = days::civil_from_days(first);
            ((qm - 1) / 3 + 1, y)
        }
        _ => return None,
    };

    let first = quarter_start(year, q);
    let last = days::add_months_daynum(first, 3) - 1;
    let (fy, fm, _) = days::civil_from_days(first);
    let name = format!("Q{} {fy}", (fm - 1) / 3 + 1);
    Some(vec![Spec::Period {
        id: format!("q-{}", days::iso_date(first)),
        from: first,
        to: last,
        name: name.clone(),
        word: "quarter",
        action: "Copy Range",
        detail: format!("{fy}-Q{}", (fm - 1) / 3 + 1),
    }])
}

/// "this weekend", "next weekend", "weekend". Saturday and Sunday, and on a
/// Sunday "this weekend" is the one being stood in rather than the one six
/// days out.
pub(super) fn try_weekend(text: &str, today: i64, _today_year: i64) -> Option<Vec<Spec>> {
    let t = normalize_words(text);
    let shift = match t.as_str() {
        "this weekend" | "weekend" => 0,
        "next weekend" => 7,
        "last weekend" => -7,
        _ => return None,
    };
    let dow = days::weekday_u(today);
    let sat = if dow == 7 {
        today - 1
    } else {
        today + (6 - dow)
    } + shift;
    let sun = sat + 1;
    Some(vec![Spec::Period {
        id: format!("weekend-{}", days::iso_date(sat)),
        from: sat,
        to: sun,
        name: format!("The weekend of {}", days::day_month(sat)),
        word: "weekend",
        action: "Copy Range",
        detail: String::new(),
    }])
}

/// "december 2027", "2027-12", "12/2027". A month and a year is a stretch of
/// days, not a day: `date -d "december 2027"` answers with the 20th, because
/// the day of the month is missing and it fills it in from today. That is the
/// wrong kind of confident, so the whole month is the answer instead.
///
/// A bare month name is refused. `date:` answers unscoped and "may", "march"
/// and "august" are words before they are months.
pub(super) fn try_month(text: &str, _today: i64, _today_year: i64) -> Option<Vec<Spec>> {
    let t = normalize_words(text);
    let (y, m);
    // YYYY-MM or YYYY/MM, the month two digits — "2027-1" is not a month.
    if let Some((a, b)) = t.split_once(['/', '-'])
        && a.len() == 4
        && b.len() == 2
        && a.chars().all(|c| c.is_ascii_digit())
        && b.chars().all(|c| c.is_ascii_digit())
        && let (Ok(yy), Ok(mm)) = (a.parse::<i64>(), b.parse::<i64>())
        && (1..=12).contains(&mm)
    {
        (y, m) = (yy, mm);
    } else if let Some((a, b)) = t.split_once('/')
        && b.len() == 4
        && a.len() <= 2
        && a.chars().all(|c| c.is_ascii_digit())
        && b.chars().all(|c| c.is_ascii_digit())
        && let (Ok(mm), Ok(yy)) = (a.parse::<i64>(), b.parse::<i64>())
        && (1..=12).contains(&mm)
    {
        (y, m) = (yy, mm);
    } else {
        // "december 2027" / "2027 december" — word is a month-name prefix.
        let mut it = t.split_whitespace();
        let (wa, wb) = (it.next()?, it.next()?);
        if it.next().is_some() {
            return None;
        }
        if let Ok(yy) = wa.parse::<i64>()
            && wa.len() == 4
            && let Some(mm) = month_of_word(wb)
        {
            (y, m) = (yy, mm);
        } else if let Ok(yy) = wb.parse::<i64>()
            && wb.len() == 4
            && let Some(mm) = month_of_word(wa)
        {
            (y, m) = (yy, mm);
        } else {
            return None;
        }
    }
    let first = days::days_from_civil(y, m, 1);
    let last = first + days::days_in_month(y, m) - 1;
    Some(vec![Spec::Period {
        id: format!("month-{y:04}-{m:02}"),
        from: first,
        to: last,
        name: format!("{} {y}", days::MONTHS[(m - 1) as usize]),
        word: "month",
        action: "Copy Range",
        detail: format!("{y:04}-{m:02}"),
    }])
}

/// "next friday" has two honest readings on a Thursday: tomorrow, and the
/// Friday of the week after. `date -d` picks the first one. Rather than argue
/// with it, both are on screen, the one `date -d` picked leading.
pub(super) fn try_next_weekday(text: &str, today: i64, _today_year: i64) -> Option<Vec<Spec>> {
    let t = normalize_words(text);
    let rest = t.strip_prefix("next ")?;
    let w = DAY_NAMES.iter().position(|d| *d == rest)?;
    let want = (w as i64 + 1) % 7;
    let dow = days::weekday_of(today);
    let mut delta = (want - dow + 7) % 7;
    if delta == 0 {
        delta = 7;
    }
    let first = today + delta;
    Some(vec![
        Spec::Day {
            day: first,
            label: String::new(),
            lead: true,
            note: None,
        },
        Spec::Day {
            day: first + 7,
            label: "The one after".to_string(),
            lead: false,
            note: None,
        },
    ])
}
