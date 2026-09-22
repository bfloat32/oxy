//! `unit:` — a unit conversion through qalc. A port of `bin/oxy-unit`.
//!
//!   unit:20 miles in km        one answer, large
//!   unit:180f in c             the same, with the units people actually type
//!   unit:how many feet in a mile   the same question asked backwards
//!   unit:6ft2 in cm            a height, which is two numbers and one answer
//!   unit:1 cup of flour in g   a volume asked as a weight, through a density
//!   unit:20 miles              km, metres and feet, without being asked
//!
//! The launcher already runs qalc for bare arithmetic, so why a second
//! keyword: `calc` has to gate hard, because `qalc -t firefox` answers "0 B"
//! and `qalc -t zzz` answers "z^3" with exit code 0 either way. That gate
//! needs a digit plus an operator or a conversion word, which throws away
//! "20 miles". A keyword is its own gate, so this side can be permissive
//! about the sentence and go looking for the conversions nobody bothered to
//! ask for.
//!
//! Permissive about the sentence, not about the units. qalc will make a unit
//! out of any letters you give it: "how many feet in a mile" came back as
//! "0.0000189394ny a·B·mi", "5 KM IN MILES" came back as "5 K", and "1 cup of
//! flour in grams" came back as "3.98529E−40 g·B²·L²" — every one of them a
//! confident wrong answer with exit code 0. So both sides of a conversion
//! have to be a unit the table has a name for, and both have to be in the
//! same family, or the answer is silence.
//!
//! One qalc process answers every expression at once through `-f`, because a
//! hero, a rate and four family rows used to be six process starts on every
//! keystroke. The second run — the only reason this provider ever starts
//! qalc twice — is the "feet and inches" question that wants autoconversion
//! left on.

mod parse;
mod table;

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::util::{on_path, shq};
use crate::provider::process;
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

use self::parse::{Kind, Question, is_space, parse};
use self::table::{family_of, pretty, targets_for};

pub struct Unit;

/// The worker's own timeout is four seconds; the batch (and the rare second
/// autoconversion run, spawned alongside it) get three.
const QALC_TIMEOUT: Duration = Duration::from_secs(3);

// ------------------------------------------------------------ the answers
//
// One qalc, every expression. `conv none` turns off autoconversion, without
// which "5 kg to pound" answers "11 lb + 0.369810 oz", which is correct and
// is not a number anybody can use.

/// Which expression a qalc output line belongs to.
enum Leg {
    Answer,
    Rate,
    More,
}

/// The expression list and what each line means, in the script's order:
/// answer, rate, then the family.
fn plan(q: &Question) -> Vec<(String, Leg)> {
    // The left side as an expression qalc can weigh, density included.
    let bridge = |want: &str| -> String {
        if let Some(d) = q.density {
            if q.sfam == "volume" && want == "mass" {
                return format!("({} {}) * ({} g/mL)", q.quantity, q.src, d);
            }
            if q.sfam == "mass" && want == "volume" {
                return format!("({} {}) / ({} g/mL)", q.quantity, q.src, d);
            }
        }
        format!("{} {}", q.quantity, q.src)
    };

    let mut exprs: Vec<(String, Leg)> = Vec::new();
    if let Some(tgt) = q.tgt {
        if q.kind == Kind::Currency {
            exprs.push((format!("{} {} to {}", q.quantity, q.src, tgt), Leg::Answer));
            exprs.push((format!("1 {} to {}", q.src, tgt), Leg::Rate));
        } else {
            let tfam = family_of(tgt).unwrap_or_default();
            exprs.push((format!("{} to {}", bridge(tfam), tgt), Leg::Answer));
            if q.sfam != "temperature" && q.density.is_none() {
                exprs.push((format!("1 {} to {}", q.src, tgt), Leg::Rate));
            }
            for t in targets_for(q.sfam, q.src) {
                if t == tgt || t == q.src {
                    continue;
                }
                exprs.push((
                    format!("{} to {}", bridge(family_of(t).unwrap_or_default()), t),
                    Leg::More,
                ));
            }
        }
    } else {
        if q.kind == Kind::Currency {
            return Vec::new();
        }
        for t in targets_for(q.sfam, q.src) {
            if t == q.src {
                continue;
            }
            exprs.push((format!("{} {} to {}", q.quantity, q.src, t), Leg::More));
        }
    }
    exprs
}

/// `qalc -t -m 200 -set "conv none" -f <file>` — the script's invocation
/// verbatim: one process, every expression, one line in one line out.
async fn run_batch(exprs: &[String]) -> Option<Vec<String>> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "oxy-unit-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    if std::fs::write(&path, format!("{}\n", exprs.join("\n"))).is_err() {
        return None;
    }
    let body = format!(
        "qalc -t -m 200 -set 'conv none' -f {}",
        quote(&path.to_string_lossy())
    );
    let done = process::run(&body, QALC_TIMEOUT).await;
    let _ = std::fs::remove_file(&path);
    done.map(|f| f.stdout.lines().map(str::to_string).collect())
}

/// qalc echoes its input when it does not understand, and exits 0 doing it,
/// so an answer that only restates the question is not an answer.
fn usable(o: &str, e: &str) -> bool {
    let o = o.trim_matches(is_space);
    if o.is_empty() {
        return false;
    }
    if o.to_lowercase() == e.to_lowercase() {
        return false;
    }
    if o.contains("rror") {
        return false;
    }
    o.chars().any(|c| c.is_ascii_digit())
}

/// qalc writes fl_oz, cal_th and min^-1. Nobody else does.
fn readable(o: &str) -> String {
    o.replace("fl_oz", "fl oz")
        .replace("cal_th", "cal")
        .replace("gal_UK", "imp gal")
        .replace("s_ton", "short ton")
        .replace("l_ton", "long ton")
        .replace("min^−1", "/min")
        .replace("min^-1", "/min")
}

/// The jq row: id, title, subtitle, detail, exec, score, view, glyph, and
/// the two copy actions — copying the answer and remembering both sides of
/// the question are one gesture.
fn row(id: &str, title: &str, subtitle: &str, detail: &str, view: &str, score: i64) -> Value {
    let copy = format!("printf %s {} | wl-copy", shq(title));
    let both = format!(
        "printf %s {} | wl-copy",
        shq(&format!("{subtitle} = {title}"))
    );
    json!({
        "id": id,
        "title": title,
        "subtitle": subtitle,
        "detail": detail,
        "exec": copy,
        "score": score,
        "view": view,
        "glyph": "",
        "actions": [
            { "title": "Copy Answer", "shortcut": "↵", "exec": copy },
            { "title": "Copy Both Sides", "exec": both },
        ],
    })
}

/// Attach the qalc lines to the plan, shape the rows. One line in, one line
/// out — if qalc broke that promise the rows would be attached to the wrong
/// questions, so a count mismatch is silence.
fn assemble(
    q: &mut Question,
    plan: &[(String, Leg)],
    out: &[String],
    mixed_out: Option<&str>,
) -> NativeOutcome {
    let mut answer = String::new();
    let mut rate = String::new();
    let mut extras: Vec<String> = Vec::new();
    for (i, (expr, leg)) in plan.iter().enumerate() {
        let o = out[i].trim_matches(is_space);
        if !usable(o, expr) {
            continue;
        }
        match leg {
            Leg::Answer => answer = readable(o),
            Leg::Rate => rate = format!("1 {} = {}", pretty(q.src), readable(o)),
            Leg::More => extras.push(readable(o)),
        }
    }

    let detail_with_rate = |notes: &[String]| -> String {
        let mut d = notes.join(" · ");
        if !rate.is_empty() {
            if !d.is_empty() {
                d.push_str(" · ");
            }
            d.push_str(&rate);
        }
        d
    };

    if let Some(tgt) = q.tgt {
        if let Some(mo) = mixed_out
            && !mo.is_empty()
            && usable(mo, &format!("{} {} to {}", q.quantity, q.src, tgt))
        {
            answer = readable(mo);
            q.notes
                .push(format!("answered in {} and what is left over", pretty(tgt)));
        }
        if answer.is_empty() {
            return NativeOutcome::Empty;
        }
        let detail = detail_with_rate(&q.notes);
        let mut rows = vec![row(
            "unit-answer",
            &answer,
            &q.as_typed,
            &detail,
            "hero",
            99000,
        )];
        let mut score = 94000i64;
        for e in &extras {
            rows.push(row(
                &format!("unit-more-{score}"),
                e,
                &q.as_typed,
                "",
                "list",
                score,
            ));
            score -= 100;
        }
        return NativeOutcome::Rows(rows);
    }

    if extras.is_empty() {
        return NativeOutcome::Empty;
    }
    let mut rows = Vec::new();
    let mut score = 95000i64;
    let mut first = true;
    for e in &extras {
        let detail = if first {
            q.notes.join(" · ")
        } else {
            String::new()
        };
        rows.push(row(
            &format!("unit-more-{score}"),
            e,
            &q.as_typed,
            &detail,
            "list",
            score,
        ));
        first = false;
        score -= 100;
    }
    NativeOutcome::Rows(rows)
}

impl NativeExt for Unit {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move {
            // The manifest's `when` is `command -v qalc`, but a native is
            // asked even when the gate fails — a missing qalc declines to
            // the script, which declines the same way.
            if !on_path("qalc") {
                return NativeOutcome::Fallback;
            }
            let Some(mut q) = parse(&ctx.arg) else {
                return NativeOutcome::Empty;
            };
            let plan = plan(&q);
            if plan.is_empty() {
                return NativeOutcome::Empty;
            }
            let exprs: Vec<String> = plan.iter().map(|(e, _)| e.clone()).collect();

            // The batch and the one autoconversion run it might need are
            // independent processes — ask both at once, the way two script
            // runs could not.
            let batch = run_batch(&exprs);
            let mixed_run = async {
                if q.mixed
                    && let Some(tgt) = q.tgt
                {
                    let e = format!("{} {} to {}", q.quantity, q.src, tgt);
                    return process::run(&format!("qalc -t -m 200 -- {}", quote(&e)), QALC_TIMEOUT)
                        .await
                        .map(|f| f.stdout);
                }
                None
            };
            let (batch, mixed) = tokio::join!(batch, mixed_run);
            let Some(out) = batch else {
                return NativeOutcome::Empty;
            };
            if out.len() != exprs.len() {
                return NativeOutcome::Empty;
            }
            let mixed_out = mixed.map(|s| s.replace('\n', " ").trim_matches(is_space).to_string());
            assemble(&mut q, &plan, &out, mixed_out.as_deref())
        })
    }
}

#[cfg(test)]
mod tests;
