//! `alarm` — a reminder set in the words people actually say, armed on a
//! systemd user timer. A port of `bin/oxy-alarm`.
//!
//!   alarm:5m tea                        a duration, then what it is for
//!   alarm:30 minutes and 45 seconds     seconds round UP — an alarm that
//!                                       fires early is a broken alarm
//!   alarm:at 7 / 19:30 / tomorrow 8     a wall clock, resolved by `date`
//!   alarm:                              what is pending, and how long is left
//!
//! The reminders are Omarchy's — `omarchy reminder <minutes> <message>` arms
//! a transient systemd user timer, so one set here is the same object as one
//! set from the bar and cancels the same way (the cancel rows still exec the
//! script's own `oxy-alarm --cancel <unit>`; the script stays installed).
//!
//! The two rules a port must not "fix" (PORTING-BACKLOG §6.6): a duration
//! that does not divide is rounded up to a whole minute and the row says
//! what it rounded to; and a time with no words after it is refused —
//! `omarchy reminder 25` stores "25-min reminder", a notification that
//! arrives knowing nothing.
//!
//! The split: `words` is the duration vocabulary, `parse` walks the front of
//! the query as a duration, `clock` reads a wall clock off the front and
//! resolves it through `date`, `render` draws the rows.

mod clock;
mod parse;
mod render;
mod words;

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::util::on_path;
use crate::provider::{Ctx, NativeExt, NativeOutcome, process};

use self::parse::Parsed;

pub struct Alarm;

/// The hero row (or the needs-a-message refusal) plus whatever is already
/// armed, listed under it — arming a second reminder is done while looking
/// at the first.
async fn emit(message: &str, subtitle: String, detail: String, minutes: i64) -> NativeOutcome {
    let mut rows = Vec::new();
    if message.trim().is_empty() {
        rows.push(render::needs_row(&subtitle));
    } else {
        rows.push(render::hero_row(message, &subtitle, &detail, minutes));
    }
    rows.extend(pending().await);
    NativeOutcome::Rows(rows)
}

/// The "in N minutes" answer: the seconds round up to a whole minute — never
/// down — and `date` says where the minute lands on the wall.
async fn duration_answer(seconds: i64, message: String) -> NativeOutcome {
    let minutes = (seconds + 59) / 60;
    let minutes = minutes.max(1);
    // `date -d "+N minutes" +%H:%M` failing means the whole answer fails,
    // as the script's `|| exit 0` did.
    let Some(fire) = clock::plus_minutes(minutes).await else {
        return NativeOutcome::Empty;
    };
    let (subtitle, detail) = render::duration_lines(minutes, seconds, &fire);
    emit(&message, subtitle, detail, minutes).await
}

/// The "at HH:MM" answer: the same row, told the way the clock was asked.
async fn clock_answer(res: clock::Answer, message: String) -> NativeOutcome {
    let minutes = (res.total + 59) / 60;
    let minutes = minutes.max(1);
    let (subtitle, detail) = clock::lines(&res.fire, &res.rel, res.rolled, minutes);
    emit(&message, subtitle, detail, minutes).await
}

/// `omarchy reminder show --json` — the read stays Omarchy's. A failed or
/// empty answer lists nothing, as the script's `|| return 0` did.
async fn pending() -> Vec<Value> {
    let Some(fin) = process::run("omarchy reminder show --json", Duration::from_secs(3)).await
    else {
        return Vec::new();
    };
    if fin.code != Some(0) || fin.stdout.is_empty() {
        return Vec::new();
    }
    render::pending_from_json(&fin.stdout)
}

impl NativeExt for Alarm {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move {
            // The manifest's `when` is `command -v omarchy`; natives are
            // asked even when `when` fails, so the gate is re-checked here.
            if !on_path("omarchy") {
                return NativeOutcome::Fallback;
            }
            let arg = ctx.arg.clone();
            // `alarm:` on its own is the pending list and nothing else — a
            // "no reminders" placeholder is an alarm-shaped thing with no
            // alarm in it.
            if arg.is_empty() {
                let rows = pending().await;
                return if rows.is_empty() {
                    NativeOutcome::Empty
                } else {
                    NativeOutcome::Rows(rows)
                };
            }
            let words = parse::shell_words(&arg);
            match parse::plan(&words) {
                Parsed::Nothing => NativeOutcome::Empty,
                Parsed::Duration { seconds, message } => duration_answer(seconds, message).await,
                Parsed::Clock(spec, message) => match clock::resolve(&spec).await {
                    // `total == 0` is the script's `exit 0` — half an alarm
                    // is not an alarm. A negative or unresolvable stamp hands
                    // the same words to the duration read.
                    Some(res) if res.total == 0 => NativeOutcome::Empty,
                    Some(res) => clock_answer(res, message).await,
                    None => {
                        let d = parse::duration(&words);
                        if d.seconds == 0 {
                            NativeOutcome::Empty
                        } else {
                            duration_answer(d.seconds, parse::message_after(&words, d.used)).await
                        }
                    }
                },
            }
        })
    }
}
