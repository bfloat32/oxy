//! The rows: the hero that arms the reminder, the "what is it for" refusal,
//! and the pending list read back from `omarchy reminder show --json` — the
//! same fields the script's `jq` programs emitted, in the same order.
//!
//! Row objects stay `serde_json::Value`s and go through `to_row` like a
//! script's output, so both paths produce identical rows.

use serde_json::{Value, json};

use crate::provider::native::util::shq;
use crate::support::quote::quote;

/// `format_span` — a leftover as "30m 45s", for the rounded-up detail.
pub(super) fn format_span(secs: i64) -> String {
    let hours = secs / 3600;
    let mins = secs % 3600 / 60;
    let rest = secs % 60;
    let mut out = String::new();
    if hours > 0 {
        out += &format!("{hours}h ");
    }
    if mins > 0 {
        out += &format!("{mins}m ");
    }
    if rest > 0 {
        out += &format!("{rest}s");
    }
    out.trim_end().to_string()
}

/// `say_duration` — the rounded time said back the way it was asked for:
/// "1 hour and 30 minutes" rather than "90 minutes". A confirmation in
/// different words than the question is one more thing to check, and "in 72
/// hours" is a number a person has to divide before it means anything.
pub(super) fn say_duration(mins: i64) -> String {
    let days = mins / 1440;
    let hours = mins % 1440 / 60;
    let mins = mins % 60;
    let mut parts: Vec<String> = Vec::new();
    if days == 1 {
        parts.push("1 day".to_string());
    } else if days > 1 {
        parts.push(format!("{days} days"));
    }
    if hours == 1 {
        parts.push("1 hour".to_string());
    } else if hours > 1 {
        parts.push(format!("{hours} hours"));
    }
    if mins == 1 {
        parts.push("1 minute".to_string());
    } else if mins > 1 {
        parts.push(format!("{mins} minutes"));
    }
    match parts.len() {
        0 => "no time at all".to_string(),
        1 => parts[0].clone(),
        2 => format!("{} and {}", parts[0], parts[1]),
        _ => format!("{}, {} and {}", parts[0], parts[1], parts[2]),
    }
}

/// The duration row's strings: "Reminder in 5 minutes, at 14:32" — plus the
/// rounding confession when the count did not divide. Setting a different
/// time from the one typed without mentioning it is worse than refusing.
pub(super) fn duration_lines(minutes: i64, total: i64, fire: &str) -> (String, String) {
    let subtitle = format!("Reminder in {}, at {fire}", say_duration(minutes));
    let detail = if minutes * 60 != total {
        format!(
            "Rounded up from {}  ·  Omarchy reminders are whole minutes",
            format_span(total)
        )
    } else {
        String::new()
    };
    (subtitle, detail)
}

/// The hero row: the message is the title, the exec arms the timer, and the
/// actions offer to set it, show the rest, and clear them all.
pub(super) fn hero_row(title: &str, subtitle: &str, detail: &str, minutes: i64) -> Value {
    let exec = format!("omarchy reminder {minutes} {}", shq(title));
    json!({
        "id": "alarm",
        "title": title,
        "subtitle": subtitle,
        "detail": detail,
        "exec": exec,
        "glyph": "󰢌",
        "score": 99000,
        "view": "hero",
        "actions": [
            { "title": "Set Reminder", "shortcut": "↵", "exec": exec },
            { "title": "Show Pending", "exec": "omarchy reminder show" },
            { "title": "Cancel Every Reminder", "exec": "omarchy reminder clear" }
        ]
    })
}

/// The refusal row: the time was understood — showing what was understood is
/// how somebody sees that "0730" was read as half past seven — but a
/// reminder with nothing to say at it is not offered.
pub(super) fn needs_row(subtitle: &str) -> Value {
    json!({
        "id": "alarm-needs-message",
        "title": "What is the reminder for?",
        "subtitle": subtitle,
        "detail": "Type what it should say",
        "exec": "",
        "glyph": "󰢌",
        "score": 99000,
        "view": "hero"
    })
}

/// `list_pending`'s `jq` program, minus the fork: one `list` row per
/// reminder (`90000 - index`), each carrying `oxy-alarm --cancel <unit>` —
/// the script's own subcommand, which stays installed — and a "cancel them
/// all" row only when there is more than one to drop. Nothing pending prints
/// nothing.
pub(super) fn pending_from_json(text: &str) -> Vec<Value> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let count = v.get("count").and_then(Value::as_i64).unwrap_or(0);
    if count == 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    match v.get("reminders") {
        Some(Value::Array(reminders)) => {
            for (i, r) in reminders.iter().enumerate() {
                // jq indexes a non-object element with `.unit` by erroring
                // out; rows already emitted still stand. `null` indexes to
                // `null`, so the row forms and the launcher drops it.
                if !(r.is_object() || r.is_null()) {
                    break;
                }
                let get = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default();
                let (unit, label, remaining, at) =
                    (get("unit"), get("label"), get("remaining"), get("atTime"));
                rows.push(json!({
                    "id": unit,
                    "title": label,
                    "subtitle": format!("{remaining} left"),
                    "detail": format!("fires at {at}"),
                    "accessory": "Cancel",
                    "glyph": "󰢌",
                    "exec": format!("oxy-alarm --cancel {}", quote(unit)),
                    "score": 90000 - i as i64,
                    "view": "list",
                }));
            }
        }
        // An empty object iterates to nothing without erroring; a non-empty
        // one died on `90000 - key`, taking the whole answer with it.
        Some(Value::Object(m)) if m.is_empty() => {}
        _ => return Vec::new(),
    }
    if count > 1 {
        rows.push(json!({
            "id": "alarm-clear",
            "title": "Cancel every reminder",
            "subtitle": format!("{count} pending"),
            "exec": "omarchy reminder clear",
            "score": 10000,
            "view": "list",
        }));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans() {
        assert_eq!(format_span(1845), "30m 45s");
        assert_eq!(format_span(90), "1m 30s");
        assert_eq!(format_span(3600), "1h");
        assert_eq!(format_span(3661), "1h 1m 1s");
    }

    #[test]
    fn said_durations() {
        assert_eq!(say_duration(5), "5 minutes");
        assert_eq!(say_duration(1), "1 minute");
        assert_eq!(say_duration(90), "1 hour and 30 minutes");
        assert_eq!(say_duration(120), "2 hours");
        assert_eq!(say_duration(4320), "3 days");
        assert_eq!(say_duration(1500), "1 day and 1 hour");
        assert_eq!(say_duration(2940), "2 days and 1 hour");
        assert_eq!(say_duration(1561), "1 day, 2 hours and 1 minute");
        assert_eq!(say_duration(0), "no time at all");
    }

    #[test]
    fn duration_lines_round_up() {
        let (sub, det) = duration_lines(31, 1845, "14:32");
        assert_eq!(sub, "Reminder in 31 minutes, at 14:32");
        assert_eq!(
            det,
            "Rounded up from 30m 45s  ·  Omarchy reminders are whole minutes"
        );
        // an exact minute confesses nothing
        let (_, det) = duration_lines(5, 300, "14:32");
        assert_eq!(det, "");
    }

    #[test]
    fn printf_q() {
        // `printf '%q'` — plain when it can be, backslashes when it must
        assert_eq!(shq("tea"), "tea");
        assert_eq!(shq("tea time"), "tea\\ time");
        assert_eq!(shq("call mum"), "call\\ mum");
        assert_eq!(shq("it's"), "it\\'s");
        assert_eq!(shq("100%"), "100%");
        assert_eq!(shq("a,b"), "a\\,b");
        assert_eq!(shq("a:b"), "a:b");
        assert_eq!(shq("a!b"), "a\\!b");
        assert_eq!(shq("#tag"), "\\#tag");
        assert_eq!(shq("a#b"), "a#b");
        assert_eq!(shq("~me"), "\\~me");
        assert_eq!(shq("a~b"), "a~b");
        assert_eq!(shq("café"), "café");
        assert_eq!(shq(""), "''");
        assert_eq!(shq("a\nb"), "$'a\\nb'");
        assert_eq!(shq("a\tb"), "$'a\\tb'");
    }

    #[test]
    fn hero_row_shape() {
        let row = hero_row("tea time", "Reminder in 5 minutes, at 14:32", "", 5);
        assert_eq!(row["id"], "alarm");
        assert_eq!(row["title"], "tea time");
        assert_eq!(row["view"], "hero");
        assert_eq!(row["score"], 99000);
        assert_eq!(row["exec"], "omarchy reminder 5 tea\\ time");
        assert_eq!(row["actions"][0]["exec"], "omarchy reminder 5 tea\\ time");
        assert_eq!(row["actions"][0]["shortcut"], "↵");
    }

    #[test]
    fn needs_row_shape() {
        let row = needs_row("Reminder in 5 minutes, at 14:32");
        assert_eq!(row["id"], "alarm-needs-message");
        assert_eq!(row["title"], "What is the reminder for?");
        assert_eq!(row["detail"], "Type what it should say");
        assert_eq!(row["exec"], "");
        assert_eq!(row["view"], "hero");
    }

    #[test]
    fn pending_rows_from_omarchy_json() {
        // a canned `omarchy reminder show --json`
        let text = r#"{
            "count": 2,
            "reminders": [
                { "unit": "omarchy-reminder-5m-tea.timer", "label": "tea",
                  "remaining": "4m 12s", "atTime": "14:32" },
                { "unit": "omarchy-reminder-90m-standup.timer", "label": "standup",
                  "remaining": "1h 20m", "atTime": "16:00" }
            ]
        }"#;
        let rows = pending_from_json(text);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["id"], "omarchy-reminder-5m-tea.timer");
        assert_eq!(rows[0]["title"], "tea");
        assert_eq!(rows[0]["subtitle"], "4m 12s left");
        assert_eq!(rows[0]["detail"], "fires at 14:32");
        assert_eq!(rows[0]["accessory"], "Cancel");
        assert_eq!(
            rows[0]["exec"],
            "oxy-alarm --cancel 'omarchy-reminder-5m-tea.timer'"
        );
        assert_eq!(rows[0]["score"], 90000);
        assert_eq!(rows[0]["view"], "list");
        assert_eq!(rows[1]["score"], 89999);
        // more than one pending earns the clear-all row
        assert_eq!(rows[2]["id"], "alarm-clear");
        assert_eq!(rows[2]["title"], "Cancel every reminder");
        assert_eq!(rows[2]["subtitle"], "2 pending");
        assert_eq!(rows[2]["exec"], "omarchy reminder clear");
        assert_eq!(rows[2]["score"], 10000);
    }

    #[test]
    fn one_pending_gets_no_clear_row() {
        let text = r#"{
            "count": 1,
            "reminders": [
                { "unit": "omarchy-reminder-x.timer", "label": "tea",
                  "remaining": "1m", "atTime": "14:32" }
            ]
        }"#;
        let rows = pending_from_json(text);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], "omarchy-reminder-x.timer");
    }

    #[test]
    fn empty_and_bad_pending() {
        assert!(pending_from_json(r#"{"count": 0, "reminders": []}"#).is_empty());
        assert!(pending_from_json("").is_empty());
        assert!(pending_from_json("not json").is_empty());
        // `.reminders` that is not a list made jq error out entirely
        assert!(pending_from_json(r#"{"count": 2, "reminders": {"a": 1}}"#).is_empty());
        assert!(pending_from_json(r#"{"count": 1}"#).is_empty());
    }
}
