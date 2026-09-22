//! Civil-calendar arithmetic shared by the `cal` and `date` providers.
//!
//! Dates are day numbers — whole days since the epoch — throughout. The bash
//! legs did the same maths at noon to survive daylight-saving edges; day
//! numbers make the trick unnecessary, since a civil date never has a clock.

/// English month names, January first.
pub const MONTHS: [&str; 12] = [
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
pub const MONTHS_ABBR: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// English weekday names, Sunday first (matching [`weekday`]).
pub const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
pub const WEEKDAYS_ABBR: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Days since the epoch for a civil date (Howard Hinnant's algorithm). A
/// civil calendar fits in a few lines and asks for no date library.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
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

/// 0 = Sunday, for a civil date.
pub fn weekday(year: i64, month: i64, day: i64) -> i64 {
    (days_from_civil(year, month, day) + 4).rem_euclid(7)
}

/// 0 = Sunday, for a day number.
pub fn weekday_of(z: i64) -> i64 {
    (z + 4).rem_euclid(7)
}

/// `date +%u`: 1 = Monday … 7 = Sunday, for a day number.
pub fn weekday_u(z: i64) -> i64 {
    match weekday_of(z) {
        0 => 7,
        w => w,
    }
}

/// Days in a month, leap years counted.
pub fn days_in_month(y: i64, m: i64) -> i64 {
    debug_assert!((1..=12).contains(&m));
    match m {
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Whether `(y, m, d)` is a real date — the validity check `date -d` performs.
pub fn valid_ymd(y: i64, m: i64, d: i64) -> bool {
    (1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m)
}

/// `date +%F` — the *local* civil date. Reading the epoch as a UTC day
/// instead puts `today` a day early for the first hours of every UTC+X
/// morning (and late in a UTC−X evening), which is most of the planet.
pub fn today() -> (i64, i64, i64) {
    let d = jiff::Zoned::now().date();
    (
        i64::from(d.year()),
        i64::from(d.month()),
        i64::from(d.day()),
    )
}

/// Today as a day number.
pub fn today_num() -> i64 {
    let (y, m, d) = today();
    days_from_civil(y, m, d)
}

pub fn add_months(year: i64, month: i64, delta: i64) -> (i64, i64) {
    let total = year * 12 + (month - 1) + delta;
    (total.div_euclid(12), total.rem_euclid(12) + 1)
}

/// `date -d "DATE +N months"`: GNU overflows the day rather than clamping —
/// 31 January + 1 month is 3 March, not 28 February. Returns a day number.
pub fn add_months_daynum(z: i64, delta: i64) -> i64 {
    let (mut y, mut m, mut d) = civil_from_days(z);
    (y, m) = add_months(y, m, delta);
    while d > days_in_month(y, m) {
        d -= days_in_month(y, m);
        (y, m) = add_months(y, m, 1);
    }
    days_from_civil(y, m, d)
}

/// ISO-8601 week number, for the "week 41" subtitle.
pub fn iso_week(year: i64, month: i64, day: i64) -> i64 {
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

pub fn iso_weeks_in(year: i64) -> i64 {
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

/// The year an ISO week belongs to — `%G` to `iso_week`'s `%V`.
pub fn iso_year(z: i64) -> i64 {
    let (y, m, d) = civil_from_days(z);
    let w = iso_week(y, m, d);
    if w == 1 && m == 12 {
        y + 1
    } else if w >= 52 && m == 1 {
        y - 1
    } else {
        y
    }
}

/// `%F`: a day number as `YYYY-MM-DD`.
pub fn iso_date(z: i64) -> String {
    let (y, m, d) = civil_from_days(z);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `%-d %B %Y`: "5 August 2027".
pub fn long_date(z: i64) -> String {
    let (y, m, d) = civil_from_days(z);
    format!("{d} {} {y}", MONTHS[(m - 1) as usize])
}

/// `%-d %B`: "5 August".
pub fn day_month(z: i64) -> String {
    let (_, m, d) = civil_from_days(z);
    format!("{d} {}", MONTHS[(m - 1) as usize])
}

/// `%-d %b`: "5 Aug".
pub fn day_month_abbr(z: i64) -> String {
    let (_, m, d) = civil_from_days(z);
    format!("{d} {}", MONTHS_ABBR[(m - 1) as usize])
}

/// `%A, %-d %B %Y`: "Thursday, 5 August 2027".
pub fn full_date(z: i64) -> String {
    let (y, m, d) = civil_from_days(z);
    format!(
        "{}, {d} {} {y}",
        WEEKDAYS[weekday(y, m, d) as usize],
        MONTHS[(m - 1) as usize]
    )
}

/// Day of year — `%-j`.
pub fn day_of_year(z: i64) -> i64 {
    let (y, m, d) = civil_from_days(z);
    days_from_civil(y, m, d) - days_from_civil(y, 1, 1) + 1
}
