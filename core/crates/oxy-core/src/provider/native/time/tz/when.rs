//! "3pm tokyo", "monday 9am in london", "noon", "john:tokyo maria:spain" —
//! pulling the time and the zone out of the words, and the `date -d` work
//! the script handed to coreutils.
//!
//! jiff has no natural-language date parser, and none is needed: the forms
//! `normalize_time` can produce are a small grammar — `now`, an optional
//! `(next )?dayword`, and `H:MM[:SS][ am|pm]` — interpreted here directly.
//! `looks_like_time` is the same three patterns the script's regexes were,
//! written out as code rather than handed to `=~`.

use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::names::{self, ws};
use super::zones::Board;
use crate::provider::native::time::days;

/// The words `date` reads as a day. Order is the script's `DAYWORDS`, which
/// puts the full name before its abbreviations.
const DAYWORDS: &[&str] = &[
    "today",
    "tonight",
    "tomorrow",
    "yesterday",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "mon",
    "tue",
    "tues",
    "wed",
    "weds",
    "thu",
    "thur",
    "thurs",
    "fri",
    "sat",
    "sun",
];

/// The weekday number the diff wants: `date +%w`, Sunday 0.
fn weekday_num(word: &str) -> Option<i64> {
    Some(match word {
        "sunday" | "sun" => 0,
        "monday" | "mon" => 1,
        "tuesday" | "tue" | "tues" => 2,
        "wednesday" | "wed" | "weds" => 3,
        "thursday" | "thu" | "thur" | "thurs" => 4,
        "friday" | "fri" => 5,
        "saturday" | "sat" => 6,
        _ => return None,
    })
}

/// `^[0-9]{1,2}([:.h][0-9]{2})?(:[0-9]{2})?([[:space:]]*(am|pm))?$` — the
/// clock part on its own.
fn is_clock(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 || i > 2 {
        return false;
    }
    if i < b.len() && matches!(b[i], b':' | b'.' | b'h') {
        if i + 2 < b.len() && b[i + 1].is_ascii_digit() && b[i + 2].is_ascii_digit() {
            i += 3;
        } else {
            return false;
        }
    }
    if i < b.len() && b[i] == b':' {
        if i + 2 < b.len() && b[i + 1].is_ascii_digit() && b[i + 2].is_ascii_digit() {
            i += 3;
        } else {
            return false;
        }
    }
    let tail = s[i..].trim_start_matches(ws);
    tail.is_empty() || tail == "am" || tail == "pm"
}

/// The `[(next )?DAY (at )?]` prefix — what follows it, when the text opens
/// with a day word. `at` is eaten only when `with_at`: the third pattern in
/// `looks_like_time` has no `at`.
fn day_prefix(s: &str, with_at: bool) -> Option<&str> {
    let mut rest = s;
    if let Some(r) = rest.strip_prefix("next")
        && r.starts_with(ws)
    {
        rest = r.trim_start_matches(ws);
    }
    for dw in DAYWORDS {
        if let Some(r) = rest.strip_prefix(dw)
            && r.starts_with(ws)
        {
            let r = r.trim_start_matches(ws);
            if with_at
                && let Some(r2) = r.strip_prefix("at")
                && r2.starts_with(ws)
            {
                return Some(r2.trim_start_matches(ws));
            }
            return Some(r);
        }
    }
    None
}

/// `looks_like_time` — the three patterns, verbatim.
pub(super) fn looks_like_time(text: &str) -> bool {
    let s = text.to_lowercase();
    if matches!(s.as_str(), "now" | "noon" | "midnight" | "midday") {
        return true;
    }
    if let Some(rest) = day_prefix(&s, true)
        && is_clock(rest)
    {
        return true;
    }
    if is_clock(&s) {
        return true;
    }
    if let Some(rest) = day_prefix(&s, false)
        && matches!(rest, "noon" | "midnight" | "midday")
    {
        return true;
    }
    false
}

/// `normalize_time` — `date -d` reads "noon" as nothing and "3" as the
/// third of the month, so the two forms people type most both need
/// translating before it sees them. The output is a spec for [`eval_spec`],
/// the same string the script printed for `date`.
pub(super) fn normalize_time(text: &str) -> String {
    let s0 = text.to_lowercase();
    match s0.as_str() {
        "noon" | "midday" => return "12:00".to_string(),
        "midnight" => return "00:00".to_string(),
        "now" => return "now".to_string(),
        _ => {}
    }

    let mut s = s0.clone();
    let mut day = String::new();
    // A day in front of the clock, kept whole and handed to `date` with the
    // clock behind it: "monday 9am" was refused outright, and it is how a
    // meeting next week gets asked about.
    if let Some((raw_day, rest)) = split_day(&s0) {
        day = raw_day;
        if day == "tonight" {
            day = "today".to_string();
        }
        s = rest.to_string();
        match s.as_str() {
            "noon" | "midday" => return format!("{day} 12:00"),
            "midnight" => return format!("{day} 00:00"),
            _ => {}
        }
    }

    // "15.30" and "15h30" are the same time as "15:30" to everyone except
    // `date`.
    s = s.replace(['.', 'h'], ":");
    if s.ends_with(':') {
        s.pop();
    }
    if let Some((h, mer)) = bare_number(&s) {
        s = format!("{h}:00");
        if let Some(m) = mer {
            s = format!("{s} {m}");
        }
    }
    if !day.is_empty() {
        s = format!("{day} {s}");
    }
    s
}

/// `^(next ws+)?(DAY) ws+(at ws+)?(.+)$` — the day part is returned raw
/// (`"next  monday"` keeps its double space; `date` does not care) and the
/// rest separately.
fn split_day(s: &str) -> Option<(String, &str)> {
    let mut i = 0;
    if s.starts_with("next") && s[4..].starts_with(ws) {
        i = 4 + s[4..].find(|c: char| !ws(c)).unwrap_or(s.len() - 4);
    }
    for dw in DAYWORDS {
        if s[i..].starts_with(dw) {
            let e = i + dw.len();
            if e < s.len() && ws(s.as_bytes()[e] as char) {
                let mut j = e;
                while j < s.len() && ws(s.as_bytes()[j] as char) {
                    j += 1;
                }
                let mut rest = &s[j..];
                if let Some(r) = rest.strip_prefix("at")
                    && r.starts_with(ws)
                {
                    rest = r.trim_start_matches(ws);
                }
                if !rest.is_empty() {
                    return Some((s[..e].to_string(), rest));
                }
            }
        }
    }
    None
}

/// `^([0-9]{1,2})[[:space:]]*(am|pm)?$` — a bare number, maybe with a
/// meridian: "9" is 09:00, "5pm" is 17:00.
fn bare_number(s: &str) -> Option<(i64, Option<&'static str>)> {
    let t = s.trim_matches(ws);
    let (digits, mer) = if let Some(d) = t.strip_suffix("am") {
        (d.trim_end_matches(ws), Some("am"))
    } else if let Some(d) = t.strip_suffix("pm") {
        (d.trim_end_matches(ws), Some("pm"))
    } else {
        (t, None)
    };
    if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((digits.parse().ok()?, mer))
}

/// The clock part of a spec, the way GNU `date` accepts it: `H:M[:S]` with
/// one or two digits per part, minutes and seconds under 60, hours under
/// 24 — or `1`–`12` with an `am`/`pm` it can be glued to. Anything else is
/// `date`'s "invalid date".
fn parse_clock(s: &str) -> Option<(i8, i8, i8)> {
    let s = s.trim_end_matches(ws);
    let (body, meridian) = if let Some(b) = s.strip_suffix("am") {
        (b.trim_end_matches(ws), Some(0i64))
    } else if let Some(b) = s.strip_suffix("pm") {
        (b.trim_end_matches(ws), Some(12i64))
    } else {
        (s, None)
    };
    let parts: Vec<&str> = body.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut nums = Vec::with_capacity(3);
    for p in &parts {
        if p.is_empty() || p.len() > 2 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        nums.push(p.parse::<i64>().ok()?);
    }
    let (mut h, m, sec) = (nums[0], nums[1], if nums.len() == 3 { nums[2] } else { 0 });
    if m > 59 || sec > 59 {
        return None;
    }
    match meridian {
        Some(shift) => {
            if !(1..=12).contains(&h) {
                return None;
            }
            h = h % 12 + shift;
        }
        None if h > 23 => return None,
        None => {}
    }
    Some((h as i8, m as i8, sec as i8))
}

/// `TZ="$zone" date -d "$spec" +%s` — the spec is the small grammar
/// `normalize_time` emits: `now`, or `[next ]DAY ` + `H:MM[:SS][ am|pm]`.
/// The day offsets are GNU's: a weekday is its next occurrence (today
/// counts), `next` bumps only a same-day hit, and `next` before a relative
/// day ("next today") is no date at all.
pub(super) fn eval_spec(spec: &str, tz: &TimeZone) -> Option<i64> {
    if spec == "now" {
        return Some(Timestamp::now().as_second());
    }
    let mut rest = spec;
    let mut day_off = 0i64;
    let mut next = false;
    if let Some(r) = rest.strip_prefix("next")
        && r.starts_with(ws)
    {
        next = true;
        rest = r.trim_start_matches(ws);
    }
    for dw in DAYWORDS {
        if let Some(r) = rest.strip_prefix(dw)
            && r.starts_with(ws)
        {
            rest = r.trim_start_matches(ws);
            day_off = match weekday_num(dw) {
                // "next today" is not a date GNU can read.
                None if next => return None,
                None => match *dw {
                    "today" | "tonight" => 0,
                    "tomorrow" => 1,
                    "yesterday" => -1,
                    _ => 0,
                },
                Some(w) => {
                    let dow = day_of_today(tz);
                    let diff = (w - dow + 7) % 7;
                    if next && diff == 0 { 7 } else { diff }
                }
            };
            break;
        }
    }
    let (h, m, sec) = parse_clock(rest)?;
    let today = Timestamp::now().to_zoned(tz.clone()).date();
    let day = today.checked_add(jiff::Span::new().days(day_off)).ok()?;
    let dt = day.at(h, m, sec, 0);
    let z = dt.to_zoned(tz.clone()).ok()?;
    Some(z.timestamp().as_second())
}

/// `date +%w` in the zone — Sunday 0 — for the weekday diff.
fn day_of_today(tz: &TimeZone) -> i64 {
    let today = Timestamp::now().to_zoned(tz.clone()).date();
    days::weekday(
        i64::from(today.year()),
        i64::from(today.month()),
        i64::from(today.day()),
    )
}

/// What `parse_when` found: a time to convert, the zone it is in — either
/// may be empty.
#[derive(Default)]
pub(super) struct When {
    pub time: String,
    pub zone: String,
}

/// `parse_when` — splits "9am tokyo" into a time and the zone it is in.
/// Walking the split point from the left means the shortest time wins, so
/// "9 am tokyo" reads as 9am rather than as the 9th. The reversed pass is
/// the same sentence with the words swapped: "tokyo 3pm".
pub(super) fn parse_when(text: &str, board: &Board) -> Option<When> {
    let mut w = When::default();
    if text.is_empty() {
        return Some(w);
    }
    if !looks_like_time(text)
        && let Some(zone) = names::resolve_zone(text, board)
    {
        w.zone = zone;
        return Some(w);
    }
    if looks_like_time(text) {
        w.time = text.to_string();
        return Some(w);
    }

    let words: Vec<&str> = text.split(ws).filter(|p| !p.is_empty()).collect();
    let n = words.len();
    for i in 1..n {
        let left = words[..i].join(" ");
        if !looks_like_time(&left) {
            continue;
        }
        if let Some(zone) = names::resolve_zone(&words[i..].join(" "), board) {
            w.time = left;
            w.zone = zone;
            return Some(w);
        }
    }
    for i in (1..n).rev() {
        let left = words[..i].join(" ");
        let right = words[i..].join(" ");
        if !looks_like_time(&right) {
            continue;
        }
        if let Some(zone) = names::resolve_zone(&left, board) {
            w.time = right;
            w.zone = zone;
            return Some(w);
        }
    }
    None
}

/// `parse_places` — a list of places, not a place. "tokyo vs london",
/// "tokyo and london", "london new york tokyo": a comparison, which is the
/// question the day grid is drawn to answer. Separators are unambiguous so
/// they split first; failing those, the words are eaten greedily from the
/// left, longest run that resolves first, so "new york" stays one place.
///
/// `Some` is `(label, zone)` pairs, two or more — the script's
/// `compare_names`/`compare_zones`.
pub(super) fn parse_places(text: &str, board: &Board) -> Option<Vec<(String, String)>> {
    let mut places: Vec<(String, String)> = Vec::new();
    if has_separator(text) {
        for part in split_on_separators(text) {
            let part = part.trim_matches(ws);
            if part.is_empty() {
                continue;
            }
            let zone = names::resolve_zone(part, board)?;
            places.push((names::label_for_zone(&zone, board), zone));
        }
    } else {
        let mut rest = text;
        while !rest.is_empty() {
            let mut probe = rest;
            let mut taken: Option<(&str, String)> = None;
            while !probe.is_empty() {
                if let Some(zone) = names::resolve_zone(probe, board) {
                    taken = Some((probe, zone));
                    break;
                }
                match probe.rfind(ws) {
                    Some(p) => probe = probe[..p].trim_end_matches(ws),
                    None => break,
                }
            }
            let (took, zone) = taken?;
            places.push((names::label_for_zone(&zone, board), zone));
            rest = rest[took.len()..].trim_start_matches(ws);
        }
    }
    (places.len() >= 2).then_some(places)
}

/// The gate that chooses the separator branch:
/// `[[:space:]](vs|versus|and|or|x)[[:space:]]` anywhere, or a comma, or a
/// slash.
fn has_separator(text: &str) -> bool {
    if text.contains(',') || text.contains('/') {
        return true;
    }
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if ws(b[i] as char) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && !ws(b[i] as char) {
            i += 1;
        }
        if start > 0
            && i < b.len()
            && matches!(&text[start..i], "vs" | "versus" | "and" | "or" | "x")
        {
            return true;
        }
    }
    false
}

/// `sed 's/[[:space:]]+(vs|versus|and|or|x)[[:space:]]+/\n/g; s/[,\/]/\n/g'`
/// — the parts a list of places splits into.
fn split_on_separators(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut start = 0;
    while i < b.len() {
        if b[i] == b',' || b[i] == b'/' {
            out.push(&text[start..i]);
            i += 1;
            start = i;
            continue;
        }
        if ws(b[i] as char) {
            let ws0 = i;
            while i < b.len() && ws(b[i] as char) {
                i += 1;
            }
            let wstart = i;
            while i < b.len() && !ws(b[i] as char) && b[i] != b',' && b[i] != b'/' {
                i += 1;
            }
            let wend = i;
            if wend < b.len()
                && ws(b[wend] as char)
                && matches!(&text[wstart..wend], "vs" | "versus" | "and" | "or" | "x")
            {
                out.push(&text[start..ws0]);
                while i < b.len() && ws(b[i] as char) {
                    i += 1;
                }
                start = i;
            }
            continue;
        }
        i += 1;
    }
    out.push(&text[start..]);
    out
}

/// `^<t:([0-9]{1,12})(:[tTdDfFsSR])?>$` — a pasted Discord timestamp reads
/// as an instant, not as a search; the style is ignored on the way in
/// because it only ever described how somebody else's client drew it.
pub(super) fn discord_stamp(text: &str) -> Option<i64> {
    let body = text.strip_prefix("<t:")?.strip_suffix('>')?;
    let (digits, style) = match body.split_once(':') {
        Some((d, s)) => (d, Some(s)),
        None => (body, None),
    };
    if digits.is_empty() || digits.len() > 12 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if let Some(s) = style {
        let b = s.as_bytes();
        if b.len() != 1
            || !matches!(
                b[0],
                b't' | b'T' | b'd' | b'D' | b'f' | b'F' | b's' | b'S' | b'R'
            )
        {
            return None;
        }
    }
    digits.parse().ok()
}

/// `^<t:[0-9]*(:[a-zA-Z])?\>?$` — a paste still in progress: silence, not an
/// answer.
pub(super) fn partial_discord(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("<t:") else {
        return false;
    };
    let digits_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let rest = &rest[digits_end..];
    match rest.strip_prefix(':') {
        Some(r) => {
            let Some(c) = r.chars().next() else {
                return false;
            };
            if !c.is_ascii_alphabetic() {
                return false;
            }
            matches!(&r[1..], "" | ">")
        }
        None => matches!(rest, "" | ">"),
    }
}

/// `^(utc|gmt)?[[:space:]]*[+-][[:space:]]*[0-9]{1,2}[:.][0-9]{2}$` on the
/// lowercased text — an offset that is not a whole number of hours, which
/// no Etc zone can carry.
pub(super) fn minute_offset(text: &str) -> bool {
    let s = text.to_lowercase();
    let mut rest = s.as_str();
    for prefix in ["utc", "gmt"] {
        if let Some(r) = rest.strip_prefix(prefix) {
            rest = r;
            break;
        }
    }
    let rest = rest.trim_start_matches(ws);
    let Some(first) = rest.as_bytes().first() else {
        return false;
    };
    if !matches!(first, b'+' | b'-') {
        return false;
    }
    let rest = rest[1..].trim_start_matches(ws);
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if digits == 0 || digits > 2 || digits == rest.len() {
        return false;
    }
    let rest = &rest[digits..];
    let b = rest.as_bytes();
    b.len() == 3 && matches!(b[0], b':' | b'.') && b[1].is_ascii_digit() && b[2].is_ascii_digit()
}

/// `^[0-9]{9,11}$` — a bare unix timestamp in the range that is a plausible
/// date rather than a quantity somebody meant to convert.
pub(super) fn bare_stamp(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if !(9..=11).contains(&b.len()) || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    text.parse().ok()
}

/// `^(.+[^[:space:]])[[:space:]]+(in|to|for)[[:space:]]+([^[:space:]].*)$` —
/// the *last* joining word wins, so "3pm in tokyo in london" reads as
/// "(3pm in tokyo) in london".
pub(super) fn split_joiner(text: &str) -> Option<(String, String)> {
    let b = text.as_bytes();
    let mut best: Option<(usize, usize)> = None;
    let mut i = 0;
    while i < b.len() {
        if ws(b[i] as char) {
            i += 1;
            continue;
        }
        let wstart = i;
        while i < b.len() && !ws(b[i] as char) {
            i += 1;
        }
        let wend = i;
        if matches!(&text[wstart..wend], "in" | "to" | "for") && wstart > 0 && wend < b.len() {
            // The left side ends at the start of the whitespace run before
            // the word — a run at position 0 leaves an empty left, which is
            // no match.
            let mut l = wstart;
            while l > 0 && ws(b[l - 1] as char) {
                l -= 1;
            }
            let mut r = wend;
            while r < b.len() && ws(b[r] as char) {
                r += 1;
            }
            if l > 0 && r < b.len() {
                best = Some((l, r));
            }
        }
    }
    best.map(|(l, r)| (text[..l].to_string(), text[r..].to_string()))
}

/// `sed 's/[[:space:]]+(in|from|at)[[:space:]]+/ /'` — the first joining
/// word collapses to a space, so "3pm in tokyo" is "3pm tokyo" the second
/// time through.
pub(super) fn contract_joiner(text: &str) -> String {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if ws(b[i] as char) {
            i += 1;
            continue;
        }
        let wstart = i;
        while i < b.len() && !ws(b[i] as char) {
            i += 1;
        }
        let wend = i;
        if matches!(&text[wstart..wend], "in" | "from" | "at") && wstart > 0 && wend < b.len() {
            let mut l = wstart;
            while l > 0 && ws(b[l - 1] as char) {
                l -= 1;
            }
            let mut r = wend;
            while r < b.len() && ws(b[r] as char) {
                r += 1;
            }
            return format!("{} {}", &text[..l], &text[r..]);
        }
    }
    text.to_string()
}

/// `grep -oE '(^|[[:space:]])[A-Za-z0-9_-]+:'` — whether the text holds even
/// one `name:` shape, and where each one starts.
pub(super) fn pair_starts(text: &str) -> Vec<usize> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if (i == 0 || ws(b[i - 1] as char))
            && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'-')
        {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'-') {
                i += 1;
            }
            if i < b.len() && b[i] == b':' {
                out.push(start);
            }
            continue;
        }
        i += 1;
    }
    out
}

/// One segment per `name:` — a place may hold spaces ("new york"), so the
/// split is on the next `name:` rather than on whitespace. The text before
/// the first `name:` is the sed pass's first line and is read the same way;
/// trailing whitespace stays in the segment because `where` is trimmed the
/// way the shell trims it.
pub(super) fn pair_segments(text: &str) -> Vec<&str> {
    let starts = pair_starts(text);
    let mut out: Vec<&str> = Vec::new();
    if let Some(&first) = starts.first()
        && first > 0
    {
        out.push(&text[..first]);
    }
    out.extend(starts.iter().enumerate().map(|(k, &s)| {
        let end = starts.get(k + 1).copied().unwrap_or(text.len());
        &text[s..end]
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use serde_json::json;

    fn board() -> Board {
        Board::load(&Settings::merge(&json!({})))
    }

    #[test]
    fn times_look_like_times() {
        for t in [
            "3pm",
            "3 pm",
            "15:30",
            "9:00",
            "9 am",
            "9am",
            "12:30:45",
            "now",
            "noon",
            "midnight",
            "midday",
            "monday 9am",
            "next tuesday 5pm",
            "tomorrow noon",
            "sun at 9",
            "9.30",
            "9h30",
            "9:00pm",
            // The regexes only check the *shape* — "9" is a bare hour and
            // "25:99" is digits and a colon; whether the numbers make a clock
            // is `eval_spec`'s question, not this one's.
            "9",
            "25:99",
        ] {
            assert!(looks_like_time(t), "{t}");
        }
        for t in [
            "tokyo",
            "3pm tokyo",
            "monday",
            "next monday",
            "monday at noon",
            "maria:tokyo",
            "at noon",
        ] {
            assert!(!looks_like_time(t), "{t}");
        }
    }

    #[test]
    fn normalize_is_the_date_spec() {
        assert_eq!(normalize_time("noon"), "12:00");
        assert_eq!(normalize_time("midnight"), "00:00");
        assert_eq!(normalize_time("now"), "now");
        assert_eq!(normalize_time("3pm"), "3:00 pm");
        assert_eq!(normalize_time("9"), "9:00");
        assert_eq!(normalize_time("9.30"), "9:30");
        assert_eq!(normalize_time("9h30"), "9:30");
        assert_eq!(normalize_time("15:30"), "15:30");
        assert_eq!(normalize_time("monday 9am"), "monday 9:00 am");
        assert_eq!(normalize_time("tomorrow noon"), "tomorrow 12:00");
        assert_eq!(normalize_time("tonight 9pm"), "today 9:00 pm");
        assert_eq!(normalize_time("next tuesday 5pm"), "next tuesday 5:00 pm");
    }

    #[test]
    fn eval_reads_the_spec_the_way_date_did() {
        let utc = TimeZone::UTC;
        let a = |spec: &str| {
            eval_spec(spec, &utc).map(|s| {
                Timestamp::from_second(s)
                    .unwrap()
                    .to_zoned(utc.clone())
                    .strftime("%H:%M")
                    .to_string()
            })
        };
        assert_eq!(a("9:00 am").as_deref(), Some("09:00"));
        assert_eq!(a("9:00 pm").as_deref(), Some("21:00"));
        assert_eq!(a("9:00am").as_deref(), Some("09:00"));
        assert_eq!(a("12:00").as_deref(), Some("12:00"));
        assert_eq!(a("00:00").as_deref(), Some("00:00"));
        assert_eq!(a("12:30 am").as_deref(), Some("00:30"));
        assert_eq!(a("12:30 pm").as_deref(), Some("12:30"));
        assert_eq!(a("1:5").as_deref(), Some("01:05"));
        assert_eq!(a("24:00"), None);
        assert_eq!(a("15:00 pm"), None);
        assert_eq!(a("0:30 am"), None);
        assert_eq!(a("9:60"), None);
        assert_eq!(a("12:00:60"), None);
        assert_eq!(a("next today 9:00"), None);
    }

    #[test]
    fn weekdays_land_the_way_gnu_lands_them() {
        let utc = TimeZone::UTC;
        let now = Timestamp::now().to_zoned(utc.clone());
        let dow = days::weekday(
            i64::from(now.date().year()),
            i64::from(now.date().month()),
            i64::from(now.date().day()),
        );
        let today = days::days_from_civil(
            i64::from(now.date().year()),
            i64::from(now.date().month()),
            i64::from(now.date().day()),
        );
        let got = |spec: &str| {
            let d = Timestamp::from_second(eval_spec(spec, &utc).unwrap())
                .unwrap()
                .to_zoned(utc.clone())
                .date();
            days::days_from_civil(
                i64::from(d.year()),
                i64::from(d.month()),
                i64::from(d.day()),
            ) - today
        };
        // A weekday is its next occurrence — today counts — and `next`
        // bumps only the same-day hit.
        for (word, w) in [
            ("monday", 1i64),
            ("tuesday", 2),
            ("wednesday", 3),
            ("thursday", 4),
            ("friday", 5),
            ("saturday", 6),
            ("sunday", 0),
        ] {
            let diff = (w - dow + 7) % 7;
            assert_eq!(got(&format!("{word} 9:00")), diff, "{word}");
            assert_eq!(
                got(&format!("next {word} 9:00")),
                if diff == 0 { 7 } else { diff }
            );
        }
        assert_eq!(got("tomorrow 9:00"), 1);
        assert_eq!(got("yesterday 9:00"), -1);
        assert_eq!(got("today 9:00"), 0);
    }

    #[test]
    fn joiner_takes_the_last_one() {
        assert_eq!(
            split_joiner("3pm in tokyo in london"),
            Some(("3pm in tokyo".to_string(), "london".to_string()))
        );
        assert_eq!(
            split_joiner("tokyo to london"),
            Some(("tokyo".to_string(), "london".to_string()))
        );
        assert_eq!(split_joiner("in tokyo"), None);
        assert_eq!(split_joiner("tokyo in"), None);
        assert_eq!(split_joiner("pin inside"), None);
        assert_eq!(contract_joiner("3pm in tokyo"), "3pm tokyo");
        assert_eq!(contract_joiner("3pm tokyo"), "3pm tokyo");
    }

    #[test]
    fn pairs_split_on_the_next_name() {
        assert_eq!(
            pair_segments("john:tokyo maria:spain"),
            vec!["john:tokyo ", "maria:spain"]
        );
        assert_eq!(pair_starts("john:tokyo maria:spain").len(), 2);
        assert_eq!(pair_starts("maria:saopaulo"), vec![0]);
        assert_eq!(pair_starts("12:30").len(), 1);
        assert!(pair_starts("tokyo").is_empty());
    }

    #[test]
    fn parse_when_reads_both_orders() {
        let b = board();
        let w = parse_when("3pm tokyo", &b).unwrap();
        assert_eq!(w.time, "3pm");
        assert_eq!(w.zone, "Asia/Tokyo");
        let w = parse_when("tokyo 3pm", &b).unwrap();
        assert_eq!(w.time, "3pm");
        assert_eq!(w.zone, "Asia/Tokyo");
        let w = parse_when("tokyo", &b).unwrap();
        assert!(w.time.is_empty());
        assert_eq!(w.zone, "Asia/Tokyo");
        let w = parse_when("3pm", &b).unwrap();
        assert_eq!(w.time, "3pm");
        assert!(w.zone.is_empty());
        assert!(parse_when("zzznotazone", &b).is_none());
    }

    #[test]
    fn places_collect_two_or_more() {
        let b = board();
        let p = parse_places("tokyo vs london", &b).unwrap();
        assert_eq!(p[0].1, "Asia/Tokyo");
        assert_eq!(p[1].1, "Europe/London");
        let p = parse_places("london new york tokyo", &b).unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(p[1].1, "America/New_York");
        let p = parse_places("me tokyo", &b).unwrap();
        assert_eq!(p[0].1, b.local);
        assert!(parse_places("tokyo", &b).is_none());
    }

    #[test]
    fn stamps_and_offsets() {
        assert_eq!(discord_stamp("<t:1735689600:F>"), Some(1735689600));
        assert_eq!(discord_stamp("<t:1735689600>"), Some(1735689600));
        assert_eq!(discord_stamp("<t:1735689600:X>"), None);
        assert!(partial_discord("<t:17356"));
        assert!(partial_discord("<t:1735689600:X>"));
        assert!(!partial_discord("<t:abc>"));
        assert_eq!(bare_stamp("1735689600"), Some(1735689600));
        assert_eq!(bare_stamp("123"), None);
        assert!(minute_offset("+05:30"));
        assert!(minute_offset("utc+5:30"));
        assert!(minute_offset("gmt-3.30"));
        assert!(!minute_offset("gmt+2"));
        assert!(!minute_offset("tokyo"));
    }
}
