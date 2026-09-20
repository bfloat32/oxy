//! The clock read: "at 7", "7pm", "19:30", "tomorrow at 8", "quarter past
//! 3", "noon". `read` is the pure half — it decides whether the words open
//! with a wall clock at all. `resolve` is the impure half — where that clock
//! lands is `date`'s question, asked the same way the script asked it, so
//! the port cannot disagree with `date -d` about DST or a day name.
//!
//! A clock is only read when the query says it is one: an "at", a day in
//! front, an am/pm, a separator, "o'clock", "noon", "midnight", or four
//! digits. A bare "7" stays seven minutes — what it has always meant here
//! and what `omarchy reminder 7` means too.

use std::time::Duration;

use crate::provider::process;

use super::render;
use super::words::{unit_seconds, word_number};

/// What a clock-shaped opening read: the day as `date -d` wants it, the
/// wall time, and how many words it took.
#[derive(Debug, Clone)]
pub(super) struct Spec {
    /// "today", "tomorrow", a weekday name, or "next <weekday>" — always a
    /// phrase this module produced, never user text.
    pub day: String,
    pub hh: i64,
    pub mm: i64,
    /// Words the clock consumed; the rest of the query is the message.
    pub used: usize,
}

/// What `date` said about a clock: the fire time and how the day is said.
#[derive(Debug)]
pub(super) struct Answer {
    /// Seconds until it fires (`total` in the script).
    pub total: i64,
    /// `date -d "@stamp" +%H:%M`.
    pub fire: String,
    /// How the subtitle names the day.
    pub rel: Rel,
    /// The time had already gone today, so tomorrow's was taken.
    pub rolled: bool,
}

#[derive(Debug)]
pub(super) enum Rel {
    Today,
    Tomorrow,
    /// "Monday 5 August" — a day far enough out to need naming.
    On(String),
}

/// `day_phrase` — the day words, in the same readings `date:` gives them,
/// returned as the phrase `date -d` understands.
fn day_phrase(w: &str) -> Option<&'static str> {
    Some(match w.to_lowercase().as_str() {
        "today" | "tonight" => "today",
        "tomorrow" | "tmr" | "tmrw" => "tomorrow",
        "mon" | "monday" => "monday",
        "tue" | "tues" | "tuesday" => "tuesday",
        "wed" | "weds" | "wednesday" => "wednesday",
        "thu" | "thur" | "thurs" | "thursday" => "thursday",
        "fri" | "friday" => "friday",
        "sat" | "saturday" => "saturday",
        "sun" | "sunday" => "sunday",
        _ => return None,
    })
}

fn word_at(words: &[String], i: usize) -> String {
    words.get(i).map(|s| s.to_lowercase()).unwrap_or_default()
}

/// `^[0-9]{1,2}$` — one or two bare digits, as a number.
fn bare_digits(w: &str) -> Option<i64> {
    if (1..=2).contains(&w.len()) && w.bytes().all(|b| b.is_ascii_digit()) {
        w.parse().ok()
    } else {
        None
    }
}

/// `read_fraction_clock` — one bare hour plus whatever half-of-an-hour
/// language came before it: "quarter past 3", "half past 7", "20 to 6".
fn fraction_clock(a: &str, b: &str, c: &str) -> Option<(i64, i64)> {
    let mins = match a {
        "quarter" => 15,
        "half" => 30,
        _ => word_number(a).or_else(|| bare_digits(a))?,
    };
    if !matches!(b, "past" | "to" | "till" | "til") {
        return None;
    }
    let hour = word_number(c).or_else(|| bare_digits(c))?;
    if hour > 23 {
        return None;
    }
    let (mut hh, mut mm) = if b == "past" {
        (hour, mins)
    } else {
        ((hour + 23) % 24, 60 - mins)
    };
    if mm == 60 {
        mm = 0;
        hh = hour;
    }
    if mm > 59 {
        return None;
    }
    Some((hh, mm))
}

/// `^(1[3-9]|2[0-3])h([0-5][0-9])$` — "19h30" is a wall clock in every
/// language that writes times with an `h` in them; under thirteen the same
/// spelling is a duration and `compact_seconds` takes it.
fn h_clock(tok: &str) -> Option<(i64, i64)> {
    let (h, m) = tok.split_once('h')?;
    if !(1..=2).contains(&h.len())
        || m.len() != 2
        || !h.bytes().all(|b| b.is_ascii_digit())
        || !m.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let h: i64 = h.parse().ok()?;
    let m: i64 = m.parse().ok()?;
    ((13..=23).contains(&h) && m <= 59).then_some((h, m))
}

/// `^([0-9]{1,2})[:.]([0-9]{2})(am|pm)?$` — "7:00", "8.30", "7:00pm": a
/// colon and a dot are the same separator to a person.
fn sep_clock(tok: &str) -> Option<(i64, i64, Option<String>)> {
    let pos = tok.find([':', '.'])?;
    let h = &tok[..pos];
    let mut m = &tok[pos + 1..];
    let mut ampm = None;
    for ap in ["am", "pm"] {
        if let Some(rest) = m.strip_suffix(ap) {
            ampm = Some(ap.to_string());
            m = rest;
        }
    }
    if !(1..=2).contains(&h.len())
        || m.len() != 2
        || !h.bytes().all(|b| b.is_ascii_digit())
        || !m.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((h.parse().ok()?, m.parse().ok()?, ampm))
}

/// `^([0-9]{1,2})(am|pm)$` — "7pm", the meridiem attached.
fn h_ampm(tok: &str) -> Option<(i64, String)> {
    for ap in ["am", "pm"] {
        if let Some(h) = tok.strip_suffix(ap)
            && let Some(h) = bare_digits(h)
        {
            return Some((h, ap.to_string()));
        }
    }
    None
}

/// `^([0-9]{1,2})h$` — only reachable when a day or an "at" already said
/// which reading is wanted; without one, "14h" is fourteen hours from now.
fn bare_h(tok: &str) -> Option<i64> {
    tok.strip_suffix('h').and_then(bare_digits)
}

/// `^([01][0-9]|2[0-3])([0-5][0-9])$` — four digits: "0730" is half past
/// seven, not seven hundred and thirty minutes.
fn four_digit(tok: &str) -> Option<(i64, i64)> {
    if tok.len() == 4 && tok.bytes().all(|b| b.is_ascii_digit()) {
        let h: i64 = tok[..2].parse().ok()?;
        let m: i64 = tok[2..].parse().ok()?;
        if h <= 23 && m <= 59 {
            return Some((h, m));
        }
    }
    None
}

/// `parse_clock` — whether the words open with a wall clock, and which one.
/// Returns `None` when nothing clock-shaped is there; the duration read then
/// takes the same words from the start.
pub(super) fn read(words: &[String]) -> Option<Spec> {
    if words.is_empty() {
        return None;
    }
    let mut day = String::new(); // "" is today
    let mut explicit = false;
    let mut forced = false;
    let mut i = 0;

    if let Some(phrase) = day_phrase(&word_at(words, 0)) {
        day = phrase.to_string();
        explicit = true;
        forced = true;
        i = 1;
    } else if word_at(words, 0) == "next"
        && let Some(phrase) = day_phrase(&word_at(words, 1))
    {
        day = format!("next {phrase}");
        explicit = true;
        forced = true;
        i = 2;
    }

    if matches!(word_at(words, i).as_str(), "at" | "@" | "on") {
        explicit = true;
        forced = true;
        i += 1;
    }

    let mut tok = word_at(words, i);
    if tok.ends_with(',') {
        tok.pop();
    }
    if tok.is_empty() {
        return None;
    }

    let mut ampm = String::new();
    let mut hh: i64;
    let mut mm = 0i64;

    // "quarter past three" eats three words and settles the hour on its own.
    if let Some((h, m)) = fraction_clock(&tok, &word_at(words, i + 1), &word_at(words, i + 2)) {
        explicit = true;
        i += 3;
        if matches!(word_at(words, i).as_str(), "am" | "pm") {
            ampm = word_at(words, i);
            i += 1;
        }
        hh = h;
        mm = m;
    } else {
        match tok.as_str() {
            "noon" | "midday" => {
                hh = 12;
                explicit = true;
                i += 1;
            }
            "midnight" => {
                hh = 0;
                explicit = true;
                i += 1;
                // Midnight is the start of a day, and the one being asked
                // for is the one coming rather than the one eighteen hours
                // gone.
                if day.is_empty() {
                    day = "tomorrow".to_string();
                }
            }
            _ => {
                if let Some((h, m)) = h_clock(&tok) {
                    hh = h;
                    mm = m;
                    explicit = true;
                } else if let Some((h, m, ap)) = sep_clock(&tok) {
                    hh = h;
                    mm = m;
                    if let Some(ap) = ap {
                        ampm = ap;
                    }
                    explicit = true;
                } else if let Some((h, ap)) = h_ampm(&tok) {
                    hh = h;
                    ampm = ap;
                    explicit = true;
                } else if forced && let Some(h) = bare_h(&tok) {
                    // "at 14h" says which reading it wants.
                    hh = h;
                } else if let Some((h, m)) = four_digit(&tok) {
                    hh = h;
                    mm = m;
                    explicit = true;
                } else {
                    hh = bare_digits(&tok)?;
                }
                i += 1;
                if matches!(word_at(words, i).as_str(), "am" | "pm" | "a.m." | "p.m.") {
                    ampm = word_at(words, i).replace('.', "");
                    explicit = true;
                    i += 1;
                }
                if matches!(word_at(words, i).as_str(), "o'clock" | "oclock") {
                    explicit = true;
                    i += 1;
                }
            }
        }
    }

    if !explicit {
        return None;
    }
    // "2.50 hours" is a count with a decimal point in it, not ten to three.
    // The unit behind the number is what settles it.
    if unit_seconds(&word_at(words, i)).is_some() {
        return None;
    }

    match ampm.as_str() {
        "pm" if hh < 12 => hh += 12,
        "am" if hh == 12 => hh = 0,
        _ => {}
    }
    if hh > 23 || mm > 59 {
        return None;
    }

    Some(Spec {
        day: if day.is_empty() {
            "today".to_string()
        } else {
            day
        },
        hh,
        mm,
        used: i,
    })
}

/// `date -d "<day> HH:MM"` — where a wall clock lands, rolls included. One
/// bash spawn plays the script's lines verbatim: a time that has already
/// gone today is tomorrow's, a weekday that has gone this week is next
/// week's, and a stamp that still lands in the past is no clock at all —
/// the duration read then gets the words.
///
/// Prints `stamp`, `rolled`, `total`, `HH:MM`, `today|tomorrow|on <when>` —
/// five lines, or nothing when the clock does not resolve.
pub(super) async fn resolve(spec: &Spec) -> Option<Answer> {
    let hm = format!("{:02}:{:02}", spec.hh, spec.mm);
    // `day` is a phrase this module produced ("today", "monday", "next
    // monday"), `hm` is digits and a colon — nothing user-shaped is quoted.
    let body = format!(
        "stamp=$(date -d '{day} {hm}' +%s 2>/dev/null); \
         [ -n \"$stamp\" ] || exit 0; \
         now=$(date +%s); rolled=0; \
         if [ \"$stamp\" -le \"$now\" ]; then \
           case '{day}' in \
             today) stamp=$(date -d 'tomorrow {hm}' +%s 2>/dev/null); \
                    [ -n \"$stamp\" ] || exit 0; rolled=1 ;; \
             tomorrow) ;; \
             *) stamp=$((stamp + 604800)) ;; \
           esac; \
         fi; \
         total=$((stamp - now)); \
         [ \"$total\" -lt 0 ] && exit 0; \
         fire=$(date -d \"@$stamp\" +%H:%M 2>/dev/null); \
         [ -n \"$fire\" ] || exit 0; \
         target=$(date -d \"@$stamp\" +%F); \
         when=$(date -d \"@$stamp\" '+%A %-d %B'); \
         if [ \"$target\" = \"$(date +%F)\" ]; then rel=today; \
         elif [ \"$target\" = \"$(date -d tomorrow +%F)\" ]; then rel=tomorrow; \
         else rel=\"on $when\"; fi; \
         printf '%s\\n%s\\n%s\\n%s\\n%s\\n' \"$stamp\" \"$rolled\" \"$total\" \"$fire\" \"$rel\"",
        day = spec.day,
        hm = hm
    );
    let fin = process::run(&body, Duration::from_secs(2)).await?;
    if fin.code != Some(0) {
        return None;
    }
    let mut lines = fin.stdout.lines();
    let _stamp: i64 = lines.next()?.parse().ok()?;
    let rolled = lines.next()? == "1";
    let total: i64 = lines.next()?.parse().ok()?;
    let fire = lines.next()?.to_string();
    let rel = match lines.next()? {
        "today" => Rel::Today,
        "tomorrow" => Rel::Tomorrow,
        other => Rel::On(other.strip_prefix("on ").unwrap_or(other).to_string()),
    };
    Some(Answer {
        total,
        fire,
        rel,
        rolled,
    })
}

/// `date -d "+N minutes" +%H:%M` — the "at HH:MM" half of a duration row.
pub(super) async fn plus_minutes(minutes: i64) -> Option<String> {
    let fin = process::run(
        &format!("date -d '+{minutes} minutes' +%H:%M"),
        Duration::from_secs(2),
    )
    .await?;
    if fin.code != Some(0) {
        return None;
    }
    let fire = fin.stdout.trim();
    (!fire.is_empty()).then(|| fire.to_string())
}

/// The clock row's strings: "Reminder at 07:00 today, in 45 minutes". The
/// roll to tomorrow is a decision, not a detail, so it is written out — a
/// row that only said "at 07:00" would look identical whether it meant the
/// seven o'clock forty minutes away or the one twenty-three hours away.
pub(super) fn lines(fire: &str, rel: &Rel, rolled: bool, minutes: i64) -> (String, String) {
    let said = render::say_duration(minutes);
    let subtitle = match rel {
        Rel::Today => format!("Reminder at {fire} today, in {said}"),
        Rel::Tomorrow => format!("Reminder at {fire} tomorrow, in {said}"),
        Rel::On(when) => format!("Reminder at {fire} on {when}, in {said}"),
    };
    let detail = if rolled {
        format!("{fire} has already gone today, so this is tomorrow")
    } else {
        String::new()
    };
    (subtitle, detail)
}

#[cfg(test)]
mod tests {
    use super::super::parse::{message_after, shell_words};
    use super::*;

    /// (day, hh, mm, message) the way the case files pin them.
    fn clk(arg: &str) -> (String, i64, i64, String) {
        let words = shell_words(arg);
        let spec = read(&words).unwrap_or_else(|| panic!("{arg:?}: no clock read"));
        (spec.day, spec.hh, spec.mm, message_after(&words, spec.used))
    }

    fn no_clock(arg: &str) {
        assert!(read(&shell_words(arg)).is_none(), "{arg:?} read a clock");
    }

    // ------------------------------------------------- the clock cases

    #[test]
    fn case_at_7() {
        // "at 7" is a clock, and it used to be nothing at all
        let (day, hh, mm, msg) = clk("at 7 wake up");
        assert_eq!(
            (day.as_str(), hh, mm, msg.as_str()),
            ("today", 7, 0, "wake up")
        );
        let (sub, _) = lines("07:00", &Rel::Today, false, 45);
        assert_eq!(sub, "Reminder at 07:00 today, in 45 minutes");
    }

    #[test]
    fn case_7pm() {
        let (_, hh, mm, msg) = clk("7pm dinner");
        assert_eq!((hh, mm, msg.as_str()), (19, 0, "dinner"));
        assert!(
            lines("19:00", &Rel::Today, false, 90)
                .0
                .starts_with("Reminder at 19:00 ")
        );
    }

    #[test]
    fn case_7_pm_as_own_word() {
        // am/pm as its own word — the script once set a seven-minute reminder
        // called "pm dinner"
        let (_, hh, mm, msg) = clk("7 pm dinner");
        assert_eq!((hh, mm, msg.as_str()), (19, 0, "dinner"));
    }

    #[test]
    fn case_7_00_pm() {
        // a full clock time with am/pm behind it
        let (_, hh, mm, _) = clk("7:00 pm dinner");
        assert_eq!((hh, mm), (19, 0));
    }

    #[test]
    fn case_19_30() {
        // twenty-four hour time
        let (_, hh, mm, msg) = clk("19:30 standup");
        assert_eq!((hh, mm, msg.as_str()), (19, 30, "standup"));
    }

    #[test]
    fn case_8_30() {
        // a colon and a dot are the same separator to a person
        let (_, hh, mm, _) = clk("8.30 breakfast");
        assert_eq!((hh, mm), (8, 30));
    }

    #[test]
    fn case_0730() {
        // 0730 was once read as seven hundred and thirty minutes
        let (_, hh, mm, msg) = clk("0730 gym");
        assert_eq!((hh, mm, msg.as_str()), (7, 30, "gym"));
    }

    #[test]
    fn case_19h30() {
        // 19h30 is a wall clock in every language that writes one with an h
        let (_, hh, mm, _) = clk("19h30 standup");
        assert_eq!((hh, mm), (19, 30));
    }

    #[test]
    fn case_at_14h() {
        // an "at" says which reading a bare Nh wants
        let (_, hh, mm, _) = clk("at 14h gym");
        assert_eq!((hh, mm), (14, 0));
        // and without it, "14h" stays a duration
        no_clock("14h gym");
    }

    #[test]
    fn case_noon() {
        let (_, hh, mm, msg) = clk("noon lunch");
        assert_eq!((hh, mm, msg.as_str()), (12, 0, "lunch"));
    }

    #[test]
    fn case_midnight_is_tomorrows() {
        // midnight is the one coming, not the one eighteen hours gone
        let (day, hh, mm, _) = clk("midnight sleep");
        assert_eq!((day.as_str(), hh, mm), ("tomorrow", 0, 0));
        let (sub, _) = lines("00:00", &Rel::Tomorrow, false, 600);
        assert!(sub.starts_with("Reminder at 00:00 tomorrow, in "));
    }

    #[test]
    fn case_quarter_past() {
        let (_, hh, mm, msg) = clk("quarter past 3 meeting");
        assert_eq!((hh, mm, msg.as_str()), (3, 15, "meeting"));
    }

    #[test]
    fn case_half_past() {
        let (_, hh, mm, _) = clk("half past 7 gym");
        assert_eq!((hh, mm), (7, 30));
    }

    #[test]
    fn case_quarter_to() {
        // "to" counts backwards from the hour
        let (_, hh, mm, _) = clk("quarter to 6 leave");
        assert_eq!((hh, mm), (5, 45));
    }

    #[test]
    fn case_tomorrow_at_8() {
        // a day in front of the clock
        let (day, hh, mm, msg) = clk("tomorrow at 8 gym");
        assert_eq!(
            (day.as_str(), hh, mm, msg.as_str()),
            ("tomorrow", 8, 0, "gym")
        );
        let (sub, _) = lines("08:00", &Rel::Tomorrow, false, 720);
        assert!(sub.starts_with("Reminder at 08:00 tomorrow, in "));
    }

    #[test]
    fn case_tomorrow_9am() {
        // the same day without the "at"
        let (day, hh, mm, _) = clk("tomorrow 9am meeting");
        assert_eq!((day.as_str(), hh, mm), ("tomorrow", 9, 0));
    }

    #[test]
    fn case_monday_9am() {
        // a weekday that has gone this week is next week's, not a negative
        // time — `date -d monday` answers, the roll moves it
        let (day, hh, mm, msg) = clk("monday 9am standup");
        assert_eq!(
            (day.as_str(), hh, mm, msg.as_str()),
            ("monday", 9, 0, "standup")
        );
        let (sub, _) = lines(
            "09:00",
            &Rel::On("Monday 5 August".to_string()),
            false,
            5000,
        );
        assert!(sub.starts_with("Reminder at 09:00 on Monday 5 August, in "));
    }

    #[test]
    fn case_today_at_1am_rolled() {
        // a time that has gone today rolls to tomorrow and says which
        // decision it made
        let (day, hh, mm, msg) = clk("today at 1am gym");
        assert_eq!((day.as_str(), hh, mm, msg.as_str()), ("today", 1, 0, "gym"));
        let (sub, det) = lines("01:00", &Rel::Tomorrow, true, 1380);
        assert!(sub.starts_with("Reminder at 01:00 tomorrow"));
        assert!(det.contains("already gone today"));
    }

    #[test]
    fn case_at_7_alone_needs_a_message() {
        // a clock with nothing to say shows what it read, offers nothing
        let words = shell_words("at 7");
        let spec = read(&words).unwrap();
        assert_eq!((spec.hh, spec.mm, spec.used), (7, 0, 2));
        assert!(message_after(&words, spec.used).is_empty());
        let (sub, _) = lines("07:00", &Rel::Today, false, 800);
        assert!(sub.starts_with("Reminder at 07:00"));
    }

    // -------------------------------------------------------- the edges

    #[test]
    fn unit_after_the_number_unreads_the_clock() {
        // "2.50 hours" is a count with a decimal point, not ten to three
        no_clock("2.50 hours nap");
        no_clock("at 7 minutes");
    }

    #[test]
    fn bare_numbers_are_not_clocks() {
        // "5" stays five minutes; only an explicit marker makes a clock
        no_clock("5 tea");
        no_clock("7 tea");
    }

    #[test]
    fn twenty_five_hour_spelling_is_duration() {
        // "25h30" is under no clock reading — the `h` clock is 13:xx–23:xx —
        // so compact_seconds gets it (25.5 hours)
        no_clock("25h30 x");
    }

    #[test]
    fn fraction_edge_cases() {
        assert_eq!(fraction_clock("twenty", "to", "6"), Some((5, 40)));
        assert_eq!(fraction_clock("quarter", "past", "twelve"), Some((12, 15)));
        assert_eq!(fraction_clock("half", "of", "7"), None);
        assert_eq!(fraction_clock("quarter", "to", "25"), None);
    }

    #[test]
    fn day_words() {
        assert_eq!(day_phrase("tonight"), Some("today"));
        assert_eq!(day_phrase("tmrw"), Some("tomorrow"));
        assert_eq!(day_phrase("thurs"), Some("thursday"));
        assert_eq!(day_phrase("sunday"), Some("sunday"));
        assert_eq!(day_phrase("next"), None);
    }

    #[test]
    fn next_weekday_and_trailing_comma() {
        let (day, hh, mm, _) = clk("next friday 17:00 drinks");
        assert_eq!((day.as_str(), hh, mm), ("next friday", 17, 0));
        let (_, hh, mm, _) = clk("at 17:00, drinks");
        assert_eq!((hh, mm), (17, 0));
    }

    #[test]
    fn oclock_and_dotted_meridiem() {
        let (_, hh, mm, _) = clk("at 5 o'clock tea");
        assert_eq!((hh, mm), (5, 0));
        let (_, hh, mm, _) = clk("5 p.m. tea");
        assert_eq!((hh, mm), (17, 0));
    }

    // The subprocess half: only run where `bash` and GNU `date` actually
    // exist — the machines this provider is for always have both.
    fn have_date() -> bool {
        crate::provider::native::util::on_path("bash")
            && crate::provider::native::util::on_path("date")
    }

    #[tokio::test]
    async fn resolve_tomorrow_stands() {
        if !have_date() {
            return;
        }
        let spec = Spec {
            day: "tomorrow".to_string(),
            hh: 12,
            mm: 0,
            used: 0,
        };
        let res = resolve(&spec).await.expect("tomorrow 12:00 resolves");
        assert!(res.total > 0);
        assert!(!res.rolled);
        assert!(matches!(res.rel, Rel::Tomorrow));
        assert!(res.fire.len() == 5 && res.fire.contains(':'));
        let (sub, det) = lines(&res.fire, &res.rel, res.rolled, (res.total + 59) / 60);
        assert!(sub.starts_with("Reminder at 12:00 tomorrow, in "));
        assert_eq!(det, "");
    }

    #[tokio::test]
    async fn resolve_gone_time_rolls() {
        if !have_date() {
            return;
        }
        // Today at 00:00 has already gone at any wall time — the roll to
        // tomorrow is deterministic, detail and all.
        let spec = Spec {
            day: "today".to_string(),
            hh: 0,
            mm: 0,
            used: 0,
        };
        let res = resolve(&spec).await.expect("today 00:00 resolves");
        assert!(res.rolled);
        assert!(matches!(res.rel, Rel::Tomorrow));
        let (sub, det) = lines(&res.fire, &res.rel, res.rolled, (res.total + 59) / 60);
        assert!(sub.starts_with("Reminder at 00:00 tomorrow"));
        assert_eq!(det, "00:00 has already gone today, so this is tomorrow");
    }

    #[tokio::test]
    async fn plus_minutes_shapes_like_hhmm() {
        if !have_date() {
            return;
        }
        let fire = plus_minutes(5).await.expect("+5 minutes formats");
        assert_eq!(fire.len(), 5);
        assert!(fire.chars().nth(2) == Some(':'));
        assert!(fire.chars().all(|c| c.is_ascii_digit() || c == ':'));
    }
}
