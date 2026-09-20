//! `cal:` — a month, drawn as a grid by the launcher. A port of
//! `bin/oxy-calendar` for the shapes that are months and years; anything
//! natural-language (holidays, weekdays, spans) declines to `Fallback`, and
//! the declared `search` — `oxy-calendar` — answers it the way it always did.
//!
//! Row fields are the ones the `calendar` view reads: `year`, `month`,
//! `today`, `weekStart`, `marks`.

use std::future::Future;
use std::pin::Pin;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};

/// `week_start` is asked of `locale` once: the machine's locale does not
/// change between keystrokes, and a bare year asks for twelve rows — twelve
/// subprocesses per query was the alternative.
pub struct Cal {
    week_start: Option<i64>,
}

impl Default for Cal {
    fn default() -> Self {
        Self::new()
    }
}

impl Cal {
    pub fn new() -> Cal {
        Cal { week_start: None }
    }

    fn week_start(&mut self) -> i64 {
        *self.week_start.get_or_insert_with(week_start)
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const MONTHS_ABBR: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// Days since the epoch for a civil date (Howard Hinnant's algorithm). A
/// civil calendar fits in a few lines and asks for no date library.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 0 = Sunday.
fn weekday(year: i64, month: i64, day: i64) -> i64 {
    (days_from_civil(year, month, day) + 4).rem_euclid(7)
}

fn today() -> (i64, i64, i64) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    civil_from_days(secs / 86400)
}

fn add_months(year: i64, month: i64, delta: i64) -> (i64, i64) {
    let total = year * 12 + (month - 1) + delta;
    (total.div_euclid(12), total.rem_euclid(12) + 1)
}

/// The month names of the language this desktop is used in. `date -d` knew
/// the ones the locale was set to and this machine's locale was English, so
/// `cal:dezembro` was silent for the person sitting at it.
fn translate_month(query: &str) -> String {
    let (first, rest) = match query.split_once(' ') {
        Some((f, r)) => (f, r),
        None => (query, ""),
    };
    let english = match first {
        "janeiro" => "january",
        "fevereiro" => "february",
        "março" | "marco" => "march",
        "abril" => "april",
        "maio" => "may",
        "junho" => "june",
        "julho" => "july",
        "agosto" => "august",
        "setembro" => "september",
        "outubro" => "october",
        "novembro" => "november",
        "dezembro" => "december",
        _ => return query.to_string(),
    };
    if rest.is_empty() {
        english.to_string()
    } else {
        format!("{english} {rest}")
    }
}

fn month_number(name: &str) -> Option<i64> {
    let name = name.to_lowercase();
    for (i, full) in MONTHS.iter().enumerate() {
        if full.to_lowercase() == name {
            return Some(i as i64 + 1);
        }
    }
    for (i, abbr) in MONTHS_ABBR.iter().enumerate() {
        if abbr.to_lowercase() == name {
            return Some(i as i64 + 1);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn month_row(
    id: &str,
    year: i64,
    month: i64,
    tab: &str,
    score: i64,
    marks: Vec<i64>,
    note: &str,
    week_start: i64,
) -> Value {
    let (ty, tm, td) = today();
    // `today` is sent only when the month being drawn is the current one, so
    // the circle never lands on the wrong 18th.
    let today_mark = if year == ty && month == tm { td } else { 0 };
    let title = format!("{} {}", MONTHS[(month - 1) as usize], year);
    let subtitle = if note.is_empty() && today_mark > 0 {
        format!(
            "{} {}, week {}",
            WEEKDAYS[weekday(ty, tm, td) as usize],
            td,
            iso_week(ty, tm, td)
        )
    } else {
        note.to_string()
    };
    json!({
        "id": id,
        "title": title,
        "subtitle": subtitle,
        "accessory": tab,
        "year": year,
        "month": month,
        "today": today_mark,
        "weekStart": week_start,
        "marks": marks,
        // `printf '%q'` on a `-`+digits word leaves it bare; the year-month
        // this row copies is that word by construction, so the exec line is
        // byte-identical to the one `oxy-calendar` writes.
        "exec": format!("printf %s {year:04}-{month:02} | wl-copy"),
        "score": score,
        "view": "calendar",
    })
}

fn week_start() -> i64 {
    // `locale first_weekday` says it on glibc systems; anything else is
    // Sunday-first, which is also the answer the fallback gives off it.
    std::process::Command::new("locale")
        .arg("first_weekday")
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<i64>()
                .ok()
        })
        .map(|v| if v == 2 { 1 } else { 0 })
        .unwrap_or(0)
}

/// ISO-8601 week number, for the "week 41" subtitle.
fn iso_week(year: i64, month: i64, day: i64) -> i64 {
    let ordinal = days_from_civil(year, month, day) - days_from_civil(year, 1, 1) + 1;
    // The standard algorithm: week = (ordinal - weekday + 10) / 7, with the
    // edge cases of belonging to the previous or next year's week 1.
    let dow = if weekday(year, month, day) == 0 {
        7
    } else {
        weekday(year, month, day)
    };
    let week = (ordinal - dow + 10) / 7;
    if week < 1 {
        // Last ISO week of the previous year.
        return iso_weeks_in(year - 1);
    }
    if week > iso_weeks_in(year) {
        return 1;
    }
    week
}

fn iso_weeks_in(year: i64) -> i64 {
    // A year has 53 ISO weeks when it starts on Thursday, or is a leap year
    // starting on Wednesday.
    let jan1 = weekday(year, 1, 1);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    if jan1 == 4 || (leap && jan1 == 3) {
        53
    } else {
        52
    }
}

impl NativeExt for Cal {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move {
            let (ty, tm, _td) = today();
            // Resolved once and handed to every row — see the field comment.
            let ws = self.week_start();
            let mut lower = ctx.arg.trim().to_lowercase();
            lower = translate_month(&lower);

            // A month asked for by where it sits rather than by its name.
            match lower.as_str() {
                "this month" | "current month" => lower = format!("{ty}-{tm:02}"),
                "next month" | "coming month" => {
                    let (y, m) = add_months(ty, tm, 1);
                    lower = format!("{y}-{m:02}");
                }
                "last month" | "previous month" | "prev month" => {
                    let (y, m) = add_months(ty, tm, -1);
                    lower = format!("{y}-{m:02}");
                }
                "this year" => lower = ty.to_string(),
                "next year" => lower = (ty + 1).to_string(),
                "last year" => lower = (ty - 1).to_string(),
                _ => {}
            }

            if lower.is_empty() {
                let (ny, nm) = add_months(ty, tm, 1);
                let (py, pm) = add_months(ty, tm, -1);
                return NativeOutcome::Rows(vec![
                    month_row("now", ty, tm, "This month", 95000, vec![], "", ws),
                    month_row("next", ny, nm, "Next", 94000, vec![], "", ws),
                    month_row("prev", py, pm, "Last", 93000, vec![], "", ws),
                ]);
            }

            // A bare year: twelve tabs, one per month.
            if lower.len() == 4 && lower.chars().all(|c| c.is_ascii_digit()) {
                let year: i64 = lower.parse().unwrap_or(ty);
                let rows = (1..=12)
                    .map(|m| {
                        month_row(
                            &format!("m{m}"),
                            year,
                            m,
                            MONTHS_ABBR[(m - 1) as usize],
                            96000 - m * 100,
                            vec![],
                            "",
                            ws,
                        )
                    })
                    .collect();
                return NativeOutcome::Rows(rows);
            }

            // A month written as numbers: 2027-11, 11/2027, 2027/11.
            if let Some((a, b)) = lower.split_once(['/', '-'])
                && let (Ok(x), Ok(y)) = (a.parse::<i64>(), b.parse::<i64>())
            {
                let (year, month) = if a.len() == 4 {
                    (x, y)
                } else if b.len() == 4 && (1..=12).contains(&x) {
                    (y, x)
                } else {
                    (0, 0)
                };
                if (1..=12).contains(&month) && year > 0 {
                    return NativeOutcome::Rows(vec![month_row(
                        "month",
                        year,
                        month,
                        MONTHS_ABBR[(month - 1) as usize],
                        95000,
                        vec![],
                        "",
                        ws,
                    )]);
                }
            }

            // A bare month name: the next one leads, matching what
            // `date:christmas` does with a name and no year, and the other
            // year is a tab away rather than a retype.
            if lower.chars().all(|c| c.is_ascii_lowercase())
                && let Some(month) = month_number(&lower)
            {
                return NativeOutcome::Rows(if month >= tm {
                    vec![
                        month_row("m-now", ty, month, &ty.to_string(), 95000, vec![], "", ws),
                        month_row(
                            "m-next",
                            ty + 1,
                            month,
                            &(ty + 1).to_string(),
                            94000,
                            vec![],
                            "",
                            ws,
                        ),
                    ]
                } else {
                    vec![
                        month_row(
                            "m-next",
                            ty + 1,
                            month,
                            &(ty + 1).to_string(),
                            95000,
                            vec![],
                            "",
                            ws,
                        ),
                        month_row("m-now", ty, month, &ty.to_string(), 94000, vec![], "", ws),
                    ]
                });
            }

            // A month name with a year: "november 2027", "nov 2027".
            if let Some((name, year)) = lower.split_once(' ')
                && let (Some(month), Ok(year)) = (month_number(name), year.parse::<i64>())
            {
                return NativeOutcome::Rows(vec![month_row(
                    "month",
                    year,
                    month,
                    MONTHS_ABBR[(month - 1) as usize],
                    95000,
                    vec![],
                    "",
                    ws,
                )]);
            }

            // Everything else is whatever `date:` makes of it — a holiday, a
            // weekday, a week number, a quarter, a range, an explicit date —
            // and the declared `search` script reads English dates the way it
            // always has.
            NativeOutcome::Fallback
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        // 2024-02-29 is a Thursday; a leap day is the date a bad algorithm
        // gets wrong.
        let z = days_from_civil(2024, 2, 29);
        assert_eq!(civil_from_days(z), (2024, 2, 29));
        assert_eq!(weekday(2024, 2, 29), 4);
    }

    #[test]
    fn month_names() {
        assert_eq!(month_number("november"), Some(11));
        assert_eq!(month_number("nov"), Some(11));
        assert_eq!(translate_month("dezembro"), "december");
        assert_eq!(month_number("december"), Some(12));
    }
}
