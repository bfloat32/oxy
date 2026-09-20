//! The front of the query is a time; the rest is the message. `duration` is
//! the script's word-walking loop — "in"/"for" are not part of the time,
//! "a/an/half/quarter" counts and compounds, `and` between two duration
//! chunks belongs to the time and `and` before the message does not — and
//! `plan` is the fork the script took: a wall clock first, the duration read
//! when no clock was heard.
//!
//! Everything here is pure. Where the time lands on the wall is `clock`'s
//! question, asked through `date` exactly like the script asked it.

use super::clock;
use super::words::{
    compact_seconds, hundredths, is_count, starts_duration, trailing_unit, unit_seconds,
    word_number,
};

/// `read -r -a words` — the query as shell words: space, tab and newline
/// separate, empties are gone, and nothing is lowercased yet (the message
/// keeps the case it was typed in).
pub(super) fn shell_words(arg: &str) -> Vec<String> {
    arg.split([' ', '\t', '\n'])
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// `${words[*]:index}` — the rest of the sentence, one space between words
/// however it was typed.
pub(super) fn message_after(words: &[String], used: usize) -> String {
    words.get(used..).unwrap_or(&[]).join(" ")
}

fn word_at(words: &[String], i: usize) -> String {
    words.get(i).map(|s| s.to_lowercase()).unwrap_or_default()
}

/// The seconds a duration asks for, and how many words they consumed.
pub(super) struct DurSpec {
    pub seconds: i64,
    pub used: usize,
}

/// `duration` — the script's word loop. Reads "in 20 minutes", "half an
/// hour", "1h30m", "30 minutes and 45 seconds", "two and a half hours" off
/// the front; what is left is the message.
pub(super) fn duration(words: &[String]) -> DurSpec {
    let n = words.len();
    let mut i = 0;

    // "in 20 minutes" and "for 20 minutes" are how it gets said out loud,
    // and neither word is part of the time.
    while i < n && matches!(word_at(words, i).as_str(), "in" | "for") {
        i += 1;
    }

    // The unit the last chunk was counted in, so a fraction with no unit of
    // its own can borrow it — "an hour and a half" reads the "half" against
    // the hour already counted, not as the first word of the message.
    let mut last_step = 0i64;
    let mut total = 0i64;

    while i < n {
        let word = word_at(words, i);
        let next = word_at(words, i + 1);

        // "an hour", "a minute".
        if word == "a" || word == "an" {
            if let Some(step) = unit_seconds(&next) {
                total += step;
                last_step = step;
                i += 2;
                continue;
            }
            // "a quarter of an hour": the article belongs to the fraction
            // behind it.
            if next == "half" || next == "quarter" {
                i += 1;
                continue;
            }
        }

        // "half an hour", "a quarter of an hour". The filler between the
        // fraction and the unit carries no meaning, so it is walked past
        // rather than parsed.
        if word == "half" || word == "quarter" {
            let mut peek_at = i + 1;
            let mut peek = word_at(words, peek_at);
            while matches!(peek.as_str(), "of" | "a" | "an") {
                peek_at += 1;
                peek = word_at(words, peek_at);
            }
            if let Some(step) = unit_seconds(&peek) {
                total += if word == "half" { step / 2 } else { step / 4 };
                last_step = step;
                i = peek_at + 1;
                continue;
            }
            // "and a half" with nothing after it: the unit is the one
            // already counted.
            if last_step > 0 {
                total += if word == "half" {
                    last_step / 2
                } else {
                    last_step / 4
                };
                i += 1;
                continue;
            }
        }

        // A count, in digits or in words, whole or with a decimal point.
        let count_hun = if is_count(&word) {
            hundredths(&word)
        } else {
            word_number(&word).map(|sp| sp * 100)
        };

        if let Some(hun) = count_hun {
            // "two and a half hours": the fraction sits between the number
            // and its unit, so the unit is looked for past it.
            let third = word_at(words, i + 3);
            if next == "and"
                && word_at(words, i + 2) == "a"
                && matches!(third.as_str(), "half" | "quarter")
                && let Some(step) = unit_seconds(&word_at(words, i + 4))
            {
                total = total.wrapping_add(hun.wrapping_mul(step) / 100);
                total += if third == "half" { step / 2 } else { step / 4 };
                last_step = step;
                i += 5;
                continue;
            }
            if let Some(step) = unit_seconds(&next) {
                total = total.wrapping_add(hun.wrapping_mul(step) / 100);
                last_step = step;
                i += 2;
                continue;
            }
            // A bare number is minutes — what `omarchy reminder` itself takes.
            total = total.wrapping_add(hun.wrapping_mul(60) / 100);
            last_step = 60;
            i += 1;
            continue;
        }

        if let Some(step) = compact_seconds(&word) {
            total += step;
            last_step = trailing_unit(&word);
            i += 1;
            continue;
        }

        // "and" between two duration chunks is part of the time; "and" in
        // front of the message is not.
        if matches!(word.as_str(), "and" | "&" | "," | "plus")
            && total > 0
            && starts_duration(&next)
        {
            i += 1;
            continue;
        }

        break;
    }

    DurSpec {
        seconds: total,
        used: i,
    }
}

/// What the front of the query was read as. A `Clock` still has to be
/// resolved against the wall — a stamp that lands in the past hands the
/// words back to the duration read.
#[derive(Debug)]
pub(super) enum Parsed {
    /// Nothing in the query was a time — the script's silent `exit 0`.
    Nothing,
    Duration {
        seconds: i64,
        message: String,
    },
    Clock(clock::Spec, String),
}

/// The script's fork: `parse_clock` gets the words first, because "0730"
/// read as seven hundred and thirty minutes armed a reminder for a quarter
/// to ten at night. When no clock is heard, the duration read takes the
/// whole front.
pub(super) fn plan(words: &[String]) -> Parsed {
    if let Some(spec) = clock::read(words) {
        let used = spec.used;
        return Parsed::Clock(spec, message_after(words, used));
    }
    let d = duration(words);
    if d.seconds == 0 {
        Parsed::Nothing
    } else {
        Parsed::Duration {
            seconds: d.seconds,
            message: message_after(words, d.used),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::clock;
    use super::super::render;
    use super::*;

    fn plan_of(arg: &str) -> Parsed {
        plan(&shell_words(arg))
    }

    /// The duration a query reads as, and the message left over.
    fn dur(arg: &str) -> (i64, String) {
        match plan_of(arg) {
            Parsed::Duration { seconds, message } => (seconds, message),
            other => panic!("{arg:?}: wanted a duration, got {other:?}"),
        }
    }

    /// The "in N minutes" subtitle the case files pin — `fire` stands in for
    /// what `date -d "+N minutes" +%H:%M` would print on the machine.
    fn in_subtitle(seconds: i64, fire: &str) -> (String, String) {
        let minutes = (seconds + 59) / 60;
        render::duration_lines(minutes, seconds, fire)
    }

    // --------------------------------------------------- compact durations

    #[test]
    fn case_5m_tea() {
        // the shortest way to set one still works
        let (secs, msg) = dur("5m tea");
        assert_eq!(secs, 300);
        assert_eq!(msg, "tea");
        let (sub, det) = in_subtitle(secs, "14:32");
        assert_eq!(sub, "Reminder in 5 minutes, at 14:32");
        assert_eq!(det, "");
        let row = render::hero_row(&msg, &sub, &det, 5);
        assert_eq!(row["exec"], "omarchy reminder 5 tea");
    }

    #[test]
    fn case_1h30m_standup() {
        // a compound compact duration
        let (secs, msg) = dur("1h30m standup");
        assert_eq!(secs, 5400);
        assert_eq!(msg, "standup");
        let (sub, _) = in_subtitle(secs, "16:02");
        assert!(sub.contains("in 1 hour and 30 minutes"));
    }

    #[test]
    fn case_2h30_call() {
        // "2h30" is two and a half hours in a keyword that is about durations
        let (secs, msg) = dur("2h30 call");
        assert_eq!(secs, 9000);
        assert_eq!(msg, "call");
        assert!(
            in_subtitle(secs, "x")
                .0
                .contains("in 2 hours and 30 minutes")
        );
    }

    #[test]
    fn case_2h_call() {
        // a bare Nh stays a duration, or `alarm:2h` would arm for 2 in the morning
        let (secs, _) = dur("2h call");
        assert_eq!(secs, 7200);
        assert!(
            in_subtitle(secs, "x")
                .0
                .starts_with("Reminder in 2 hours, at ")
        );
    }

    #[test]
    fn case_3d_water_plants() {
        // the compact spelling of three days
        let (secs, msg) = dur("3d water plants");
        assert_eq!(secs, 259200);
        assert_eq!(msg, "water plants");
        assert!(in_subtitle(secs, "x").0.contains("in 3 days"));
    }

    #[test]
    fn case_90s_test() {
        // seconds under a minute round up to one, and say so
        let (secs, msg) = dur("90s test");
        assert_eq!(secs, 90);
        assert_eq!(msg, "test");
        let (sub, det) = in_subtitle(secs, "x");
        assert!(sub.contains("in 2 minutes"));
        assert!(det.starts_with("Rounded up from 1m 30s"));
    }

    #[test]
    fn case_decimal_compact() {
        // a decimal count with a point…
        assert_eq!(dur("1.5h call").0, 5400);
        assert!(
            in_subtitle(5400, "x")
                .0
                .contains("in 1 hour and 30 minutes")
        );
    }

    #[test]
    fn case_decimal_compact_comma() {
        // …and with a comma, which is the decimal point in half the world
        assert_eq!(dur("1,5h call").0, 5400);
    }

    // ---------------------------------------------------- worded durations

    #[test]
    fn case_30_minutes_and_45_seconds() {
        // seconds are rounded up, never down, and the row says what it rounded
        let (secs, msg) = dur("30 minutes and 45 seconds tea is ready");
        assert_eq!(secs, 1845);
        assert_eq!(msg, "tea is ready");
        let (sub, det) = in_subtitle(secs, "x");
        assert!(sub.contains("in 31 minutes"));
        assert!(det.starts_with("Rounded up from 30m 45s"));
    }

    #[test]
    fn case_twenty_minutes() {
        // a number spelled out is still a number
        let (secs, msg) = dur("twenty minutes coffee");
        assert_eq!(secs, 1200);
        assert_eq!(msg, "coffee");
        assert!(in_subtitle(secs, "x").0.contains("in 20 minutes"));
    }

    #[test]
    fn case_five_minutes() {
        assert_eq!(dur("five minutes tea").0, 300);
        assert!(in_subtitle(300, "x").0.contains("in 5 minutes"));
    }

    #[test]
    fn case_in_20_mins() {
        // "20 min" and "in 20 mins" are the same twenty minutes
        assert_eq!(dur("in 20 mins coffee").0, 1200);
        assert!(in_subtitle(1200, "x").0.contains("in 20 minutes"));
    }

    #[test]
    fn case_decimal_spelled_unit() {
        // a decimal in front of a spelled unit
        assert_eq!(dur("2.5 hours nap").0, 9000);
        assert!(
            in_subtitle(9000, "x")
                .0
                .contains("in 2 hours and 30 minutes")
        );
    }

    #[test]
    fn case_in_3_days() {
        // days are said as days, not as seventy-two hours
        assert_eq!(dur("in 3 days water plants").0, 259200);
        assert!(in_subtitle(259200, "x").0.contains("in 3 days"));
    }

    // --------------------------------------------------------- "in" fronts

    #[test]
    fn case_in_2_hours() {
        // "in" is how it gets said and is not part of the time
        let (secs, msg) = dur("in 2 hours call mum");
        assert_eq!(secs, 7200);
        assert_eq!(msg, "call mum");
        let row = render::hero_row(&msg, "", "", 120);
        assert!(
            row["exec"]
                .as_str()
                .unwrap()
                .starts_with("omarchy reminder 120 ")
        );
    }

    #[test]
    fn case_in_half_an_hour() {
        // the same fraction with the "in" in front of it
        assert_eq!(dur("in half an hour tea").0, 1800);
    }

    #[test]
    fn case_in_an_hour_and_a_half() {
        // "an hour and a half" set an hour and called the reminder "half"
        let (secs, msg) = dur("in an hour and a half call mum");
        assert_eq!(secs, 5400);
        assert_eq!(msg, "call mum");
        assert!(
            in_subtitle(secs, "x")
                .0
                .contains("in 1 hour and 30 minutes")
        );
    }

    // ------------------------------------------------------------ fractions

    #[test]
    fn case_half_an_hour() {
        // a fraction said out loud
        assert_eq!(dur("half an hour tea").0, 1800);
        assert!(in_subtitle(1800, "x").0.contains("in 30 minutes"));
    }

    #[test]
    fn case_a_quarter_of_an_hour() {
        // a quarter of an hour, with all the filler words between
        assert_eq!(dur("a quarter of an hour tea").0, 900);
        assert!(in_subtitle(900, "x").0.contains("in 15 minutes"));
    }

    #[test]
    fn case_two_and_a_half_hours() {
        // the fraction between a number and its unit
        let (secs, msg) = dur("two and a half hours nap");
        assert_eq!(secs, 9000);
        assert_eq!(msg, "nap");
        assert!(
            in_subtitle(secs, "x")
                .0
                .contains("in 2 hours and 30 minutes")
        );
    }

    // ------------------------------------------------------------- refusals

    #[test]
    fn case_5m_alone_needs_a_message() {
        // a duration with nothing to say at it shows what it read and
        // offers nothing to run
        match plan_of("5m") {
            Parsed::Duration { seconds, message } => {
                assert_eq!(seconds, 300);
                assert!(message.is_empty());
            }
            other => panic!("got {other:?}"),
        }
        let (sub, _) = in_subtitle(300, "14:32");
        let row = render::needs_row(&sub);
        assert_eq!(row["title"], "What is the reminder for?");
        assert!(
            row["subtitle"]
                .as_str()
                .unwrap()
                .starts_with("Reminder in 5 minutes")
        );
        assert_eq!(row["exec"], "");
    }

    #[test]
    fn case_bare_number_is_minutes() {
        // a bare number is minutes, which is what `omarchy reminder` takes
        match plan_of("5") {
            Parsed::Duration { seconds, message } => {
                assert_eq!(seconds, 300);
                assert!(message.is_empty());
            }
            other => panic!("got {other:?}"),
        }
        assert!(in_subtitle(300, "x").0.starts_with("Reminder in 5 minutes"));
    }

    #[test]
    fn case_plain_words_are_nothing() {
        // words with no time in them are not a reminder
        assert!(matches!(plan_of("hello world"), Parsed::Nothing));
    }

    #[test]
    fn case_an_app_name_is_nothing() {
        assert!(matches!(plan_of("firefox"), Parsed::Nothing));
    }

    #[test]
    fn case_message_is_quoted() {
        // a space in the message is not two arguments: `printf '%q'`
        let (secs, msg) = dur("5m tea time");
        assert_eq!(secs, 300);
        let row = render::hero_row(&msg, "", "", 5);
        assert_eq!(row["exec"], "omarchy reminder 5 tea\\ time");
    }

    #[test]
    fn case_actions() {
        // setting one offers to set it, show the rest, and clear them all
        let row = render::hero_row("tea", "", "", 5);
        let actions = row["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0]["title"], "Set Reminder");
        assert_eq!(actions[1]["exec"], "omarchy reminder show");
        assert_eq!(actions[2]["exec"], "omarchy reminder clear");
    }

    // ------------------------------------------- edges the loop gets right

    #[test]
    fn and_before_the_message_is_the_message() {
        // the "and" in "5m and call mum" belongs to the sentence, not the time
        let (secs, msg) = dur("5m and call mum");
        assert_eq!(secs, 300);
        assert_eq!(msg, "and call mum");
    }

    #[test]
    fn at_with_a_duration_is_silent() {
        // "at 2 hours" — the clock read refuses it (a unit follows the
        // number) and "at" opens no duration, so nothing answers.
        assert!(matches!(plan_of("at 2 hours call"), Parsed::Nothing));
    }

    #[test]
    fn a_decimal_hour_is_not_a_clock() {
        // "2.50 hours" is a count with a decimal point, not ten to three
        assert_eq!(dur("2.50 hours nap").0, 9000);
    }

    #[test]
    fn last_step_carries_for_bare_half() {
        // "an hour and a half" borrows the hour for the half
        assert_eq!(dur("an hour and a half").0, 5400);
        assert_eq!(dur("1h30m and a half").0, 5400 + 30);
    }

    #[test]
    fn in_and_for_both_skip() {
        assert_eq!(dur("for 20 minutes x").0, 1200);
        assert_eq!(dur("in in 5m x").0, 300); // every leading in/for goes
    }

    #[test]
    fn a_seventy_is_not_a_number() {
        // a word the vocabulary does not know is the message, not the count —
        // "seventy minutes" was silent on the script leg too
        assert!(matches!(plan_of("seventy minutes x"), Parsed::Nothing));
    }

    #[test]
    fn message_keeps_its_case() {
        let (_, msg) = dur("5m Tea Time");
        assert_eq!(msg, "Tea Time");
    }

    #[test]
    fn clock_specs_for_reference() {
        // exercised fully in clock.rs — a spot check that `plan` forks right
        assert!(matches!(plan_of("0730 gym"), Parsed::Clock(..)));
        assert!(matches!(plan_of("19h30 standup"), Parsed::Clock(..)));
    }

    #[test]
    fn clock_read_basics() {
        let s = clock::read(&shell_words("at 7 wake up")).unwrap();
        assert_eq!((s.hh, s.mm, s.day.as_str(), s.used), (7, 0, "today", 2));
    }
}
