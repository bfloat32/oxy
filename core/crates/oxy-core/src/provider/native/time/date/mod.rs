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

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::time::days;
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

pub struct Date;

// ------------------------------------------------------------- day maths

/// Easter Sunday, anonymous Gregorian — four of the days below hang off it
/// (Good Friday, Easter Monday, Carnival, Corpus Christi), which is why it is
/// worth the algorithm rather than a table of dates that runs out.
fn easter_of(y: i64) -> i64 {
    let a = y % 19;
    let b = y / 100;
    let c = y % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = (h + l - 7 * m + 114) % 31 + 1;
    days::days_from_civil(y, month, day)
}

/// The nth given weekday of a month: Thanksgiving is the fourth Thursday of
/// November and Mother's Day the second Sunday of May, and neither has a
/// fixed date to put in the table. `wday` is 1 = Monday … 7 = Sunday.
fn nth_weekday(y: i64, mon: i64, n: i64, wday: i64, plus: i64) -> i64 {
    let first = days::weekday_u(days::days_from_civil(y, mon, 1));
    let offset = (wday - first + 7) % 7;
    days::days_from_civil(y, mon, 1 + offset + 7 * (n - 1)) + plus
}

/// The first day of a quarter, where the quarter number may run off either
/// end of the year: "next quarter" in Q4 is Q1 of next year.
fn quarter_start(y: i64, q: i64) -> i64 {
    let mut q = q;
    let mut y = y;
    while q > 4 {
        q -= 4;
        y += 1;
    }
    while q < 1 {
        q += 4;
        y -= 1;
    }
    days::days_from_civil(y, (q - 1) * 3 + 1, 1)
}

/// Monday of the week holding a day number, ISO style.
fn monday_of(z: i64) -> i64 {
    z - (days::weekday_u(z) - 1)
}

// -------------------------------------------------------- the named days

enum Rule {
    /// A fixed date, month/day.
    Fixed(i64, i64),
    /// N days from Easter Sunday; N may be negative.
    Easter(i64),
    /// The `n`th `wday` (1 = Monday) of `month`, plus `plus` days.
    Nth(i64, i64, i64, i64),
}

struct Holiday {
    name: &'static str,
    aliases: &'static [&'static str],
    rule: Rule,
}

// Deliberately the days a person types rather than every public holiday there
// is. This desktop is in Brazil, so the national days are here beside the
// international ones and "independence day" means 7 September; anything that
// names a different date in two countries is spelled out rather than guessed,
// which is why there is no bare "father's day" (June in the US, August here).
const HOLIDAYS: &[Holiday] = &[
    Holiday {
        name: "New Year's Day",
        aliases: &[
            "new year",
            "new years",
            "new years day",
            "new year day",
            "ano novo",
        ],
        rule: Rule::Fixed(1, 1),
    },
    Holiday {
        name: "Valentine's Day",
        aliases: &[
            "valentine",
            "valentines",
            "valentines day",
            "dia dos namorados",
        ],
        rule: Rule::Fixed(2, 14),
    },
    Holiday {
        name: "Carnival",
        aliases: &["carnival", "carnaval"],
        rule: Rule::Easter(-47),
    },
    Holiday {
        name: "Ash Wednesday",
        aliases: &["ash wednesday", "quarta de cinzas"],
        rule: Rule::Easter(-46),
    },
    Holiday {
        name: "April Fools' Day",
        aliases: &["april fools", "april fool", "april fools day"],
        rule: Rule::Fixed(4, 1),
    },
    Holiday {
        name: "Good Friday",
        aliases: &["good friday", "sexta santa", "sexta-feira santa"],
        rule: Rule::Easter(-2),
    },
    Holiday {
        name: "Easter Sunday",
        aliases: &["easter", "easter sunday", "pascoa", "páscoa"],
        rule: Rule::Easter(0),
    },
    Holiday {
        name: "Easter Monday",
        aliases: &["easter monday"],
        rule: Rule::Easter(1),
    },
    Holiday {
        name: "Tiradentes",
        aliases: &["tiradentes"],
        rule: Rule::Fixed(4, 21),
    },
    Holiday {
        name: "Labour Day",
        aliases: &["labour day", "labor day", "may day", "dia do trabalho"],
        rule: Rule::Fixed(5, 1),
    },
    Holiday {
        name: "Mother's Day",
        aliases: &[
            "mother",
            "mothers",
            "mothers day",
            "dia das maes",
            "dia das mães",
        ],
        rule: Rule::Nth(5, 2, 7, 0),
    },
    Holiday {
        name: "Corpus Christi",
        aliases: &["corpus christi"],
        rule: Rule::Easter(60),
    },
    Holiday {
        name: "US Independence Day",
        aliases: &[
            "4th of july",
            "fourth of july",
            "july 4th",
            "us independence day",
        ],
        rule: Rule::Fixed(7, 4),
    },
    Holiday {
        name: "Brazilian Independence Day",
        aliases: &[
            "independence day",
            "independencia",
            "independência",
            "sete de setembro",
        ],
        rule: Rule::Fixed(9, 7),
    },
    Holiday {
        name: "Children's Day",
        aliases: &[
            "children",
            "childrens day",
            "dia das criancas",
            "dia das crianças",
        ],
        rule: Rule::Fixed(10, 12),
    },
    Holiday {
        name: "Our Lady of Aparecida",
        aliases: &["aparecida", "nossa senhora aparecida"],
        rule: Rule::Fixed(10, 12),
    },
    Holiday {
        name: "Halloween",
        aliases: &["halloween"],
        rule: Rule::Fixed(10, 31),
    },
    Holiday {
        name: "All Souls' Day",
        aliases: &["all souls", "all souls day", "finados"],
        rule: Rule::Fixed(11, 2),
    },
    Holiday {
        name: "Republic Day",
        aliases: &[
            "republic day",
            "proclamacao da republica",
            "proclamação da república",
        ],
        rule: Rule::Fixed(11, 15),
    },
    Holiday {
        name: "Black Consciousness Day",
        aliases: &[
            "black consciousness",
            "consciencia negra",
            "consciência negra",
        ],
        rule: Rule::Fixed(11, 20),
    },
    Holiday {
        name: "Thanksgiving",
        aliases: &["thanksgiving"],
        rule: Rule::Nth(11, 4, 4, 0),
    },
    Holiday {
        name: "Black Friday",
        aliases: &["black friday"],
        rule: Rule::Nth(11, 4, 4, 1),
    },
    Holiday {
        name: "Christmas Eve",
        aliases: &["christmas eve", "xmas eve", "vespera de natal"],
        rule: Rule::Fixed(12, 24),
    },
    Holiday {
        name: "Christmas Day",
        aliases: &["christmas", "xmas", "natal"],
        rule: Rule::Fixed(12, 25),
    },
    Holiday {
        name: "Boxing Day",
        aliases: &["boxing day"],
        rule: Rule::Fixed(12, 26),
    },
    Holiday {
        name: "New Year's Eve",
        aliases: &[
            "new years eve",
            "new year eve",
            "nye",
            "reveillon",
            "réveillon",
        ],
        rule: Rule::Fixed(12, 31),
    },
];

/// A rule and a year in, a day number out.
fn holiday_date(rule: &Rule, year: i64) -> i64 {
    match *rule {
        Rule::Fixed(m, d) => days::days_from_civil(year, m, d),
        Rule::Easter(off) => easter_of(year) + off,
        Rule::Nth(m, n, w, plus) => nth_weekday(year, m, n, w, plus),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Prefer {
    Next,
    Past,
}

/// Every holiday the text names, as (rank, day, name), best first.
///
/// Two ways in, and the difference is what keeps this usable while you type.
/// An exact name or alias always matches, so `nye` answers. A prefix only
/// matches from four characters, so typing towards `christmas` answers at
/// `chri` while `car` stays out of the way: without the prefix rule nothing
/// answers until the last keystroke, and with it at two characters a date row
/// lands on top of half the searches in the launcher.
///
/// Without a year the answer is the next occurrence: a holiday that has gone
/// by is almost never the one being asked about, and "christmas" in January
/// is eleven months ahead, not one behind. `Prefer::Past` is how `since`
/// looks backwards instead.
fn match_holidays(
    text: &str,
    year: i64,
    year_given: bool,
    prefer: Prefer,
    today: i64,
    today_year: i64,
) -> Vec<(i64, i64, &'static str)> {
    let mut out: Vec<(i64, i64, &'static str)> = Vec::new();
    for h in HOLIDAYS {
        // A sort key, not a flag. 0 is a name typed in full; anything else is
        // a name still being typed, ranked by how much of it is still missing.
        // "christmas" is Christmas Day exactly and Christmas Eve only by
        // accident of the Eve falling first in the year, and "chri" is four
        // letters short of one and eight short of the other.
        let mut hit: Option<i64> = None;
        if h.name.to_lowercase() == text {
            hit = Some(0);
        }
        if hit.is_none() {
            for alias in h.aliases {
                if *alias == text {
                    hit = Some(0);
                    break;
                }
                if text.len() >= 4 && alias.starts_with(text) {
                    hit = Some(1000 + (alias.len() - text.len()) as i64);
                    break;
                }
            }
        }
        if hit.is_none() && text.len() >= 4 && h.name.to_lowercase().starts_with(text) {
            hit = Some(1000 + (h.name.len() - text.len()) as i64);
        }
        let Some(hit) = hit else { continue };

        let mut day = holiday_date(&h.rule, year);
        if !year_given && year == today_year {
            // "since" looks backwards and everything else looks forwards.
            // Without this `date:days since new year` resolved New Year to the
            // one still to come and answered with a gap measured from a date
            // in the future: a confident number for a question nobody asked.
            match prefer {
                Prefer::Past if day > today => day = holiday_date(&h.rule, today_year - 1),
                Prefer::Next if day < today => day = holiday_date(&h.rule, today_year + 1),
                _ => {}
            }
        }
        out.push((hit, day, h.name));
    }
    // The script's `sort -u` on "hit|iso|name": rank first, then the earlier
    // day, then the name — and identical lines collapse.
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(b.2)));
    out.dedup();
    out
}

// --------------------------------------------------------------- resolving

/// The forms the day parser does not know. "in 90 days" is the one people
/// type most, and "a day" is the one it would answer wrong.
fn normalize_relative(t: &str) -> String {
    let mut t = t.strip_prefix("in ").unwrap_or(t).to_string();
    for suffix in [" from now", " from today"] {
        if let Some(r) = t.strip_suffix(suffix) {
            t = r.to_string();
        }
    }
    for prefix in ["an ", "a "] {
        if let Some(r) = t.strip_prefix(prefix) {
            t = format!("1 {r}");
            break;
        }
    }
    t
}

/// The words a person types that a strict grammar has never heard of,
/// rewritten into the ones it has. Ordinals are the biggest of them: "3rd of
/// march" is how a date is said out loud.
///
/// This runs after the named days have had their turn, because "4th of july"
/// is a holiday alias and stripping the "th" off it first would lose it.
fn normalize_words(t: &str) -> String {
    let t = t.trim();
    let t = t
        .strip_prefix("the ")
        .or_else(|| t.strip_prefix("on "))
        .unwrap_or(t);
    let mut out = String::with_capacity(t.len());
    for word in t.split_whitespace() {
        let w = match word {
            "tmr" | "tmrw" | "2moro" | "2mrw" => "tomorrow".to_string(),
            "yday" => "yesterday".to_string(),
            "coming" | "upcoming" => "next".to_string(),
            "mon" => "monday".to_string(),
            "tue" | "tues" => "tuesday".to_string(),
            "wed" | "weds" => "wednesday".to_string(),
            "thu" | "thur" | "thurs" => "thursday".to_string(),
            "fri" => "friday".to_string(),
            "sat" => "saturday".to_string(),
            "sun" => "sunday".to_string(),
            "sept" => "september".to_string(),
            "of" => String::new(), // "3 of march" reads as "3 march"
            _ => {
                // Ordinals: "3rd" is "3".
                let digits = word.trim_end_matches(['s', 't', 'n', 'd', 'r', 'd', 't', 'h']);
                if !digits.is_empty()
                    && digits.chars().all(|c| c.is_ascii_digit())
                    && word.len() > digits.len()
                {
                    digits.to_string()
                } else {
                    word.to_string()
                }
            }
        };
        if w.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&w);
    }
    out
}

/// d/m/y or m/d/y? A number above twelve settles it, and when both halves
/// could be a month the locale does: `d_fmt` for this machine says which way
/// round it writes them, so a Brazilian desktop reads 25/12/2026 as Christmas
/// rather than as nothing at all.
fn day_first_locale() -> bool {
    static DAY_FIRST: OnceLock<bool> = OnceLock::new();
    *DAY_FIRST.get_or_init(|| {
        // `locale d_fmt` answers the same question the script asked `%x` of a
        // known date, without formatting one. Missing locale → month-first,
        // the same fallback the script's pattern-match miss produced.
        std::process::Command::new("locale")
            .arg("d_fmt")
            .output()
            .ok()
            .and_then(|o| {
                let fmt = String::from_utf8_lossy(&o.stdout).trim().to_string();
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
fn slash_note(t: &str) -> Option<String> {
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
const DAY_NAMES: [&str; 7] = [
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
fn month_of_word(w: &str) -> Option<i64> {
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
                    // this: the one in this week, past or coming
                    0 => want - dow,
                    // next: GNU reads it as a week past the coming one
                    1 => (want - dow + 7) % 7 + 7,
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
        let secs_now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        return Some(match unit {
            // Sub-day offsets run on the wall clock like `date -d` did — "2
            // hours" near midnight is tomorrow.
            "min" => (secs_now + n * 60).div_euclid(86400),
            "hr" => (secs_now + n * 3600).div_euclid(86400),
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
fn resolve_one(text: &str, prefer: Prefer, today: i64, today_year: i64) -> Option<i64> {
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

// ---------------------------------------------------------------- phrasing

fn plural(n: i64, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

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
enum Spec {
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

fn render(spec: &Spec, today: i64) -> Value {
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

// ------------------------------------------------------------------ ranges

/// "from 1 jan to today", "1 jan to christmas", "days until christmas". The
/// gap is the answer, so the gap is the title. Both ends have to resolve or
/// this was never a range, and the single-date path gets its turn.
fn try_range(text: &str, today: i64, today_year: i64) -> Option<Vec<Spec>> {
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
fn try_week(text: &str, today: i64, _today_year: i64) -> Option<Vec<Spec>> {
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
fn try_bounds(text: &str, today: i64, today_year: i64) -> Option<Vec<Spec>> {
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
fn try_quarter(text: &str, today: i64, today_year: i64) -> Option<Vec<Spec>> {
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
fn try_weekend(text: &str, today: i64, _today_year: i64) -> Option<Vec<Spec>> {
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
fn try_month(text: &str, _today: i64, _today_year: i64) -> Option<Vec<Spec>> {
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
fn try_next_weekday(text: &str, today: i64, _today_year: i64) -> Option<Vec<Spec>> {
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
