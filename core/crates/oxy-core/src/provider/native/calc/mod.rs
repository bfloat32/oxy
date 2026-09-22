//! The calculator: a faithful port of `plugin/Calc.js`.
//!
//! qalc answers almost anything, which is the problem — `qalc -t firefox`
//! returns "0 B" with exit code 0. The gate, the rewrites and the answer
//! shaping are all here because every one of them exists to stop a wrong
//! answer that arrived wearing exit code 0.

mod answer;
mod money;
mod numbers;
pub mod unit;
mod units;

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

use fancy_regex::Regex;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};

use self::answer::{for_qalc, parse, reading, undecidable};
use self::money::{is_money, names_currency, with_currencies};
use self::numbers::{EXPONENT_FROM, precision_for};

pub struct Calc;

impl Default for Calc {
    fn default() -> Self {
        Self::new()
    }
}

impl Calc {
    pub fn new() -> Calc {
        Calc
    }
}

// --------------------------------------------------------------------- gate
//
// A digit plus either an operator or a conversion word. Both halves matter:
// "5" alone is not a question, and "a + b" is not arithmetic.
fn looks_like_math(text: &str) -> bool {
    if !text.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    if undecidable(text) {
        return false;
    }
    if operator_re().is_match(text).unwrap_or(false) {
        return true;
    }
    if conversion_re().is_match(text).unwrap_or(false) {
        return true;
    }
    money_preposition_re().is_match(text).unwrap_or(false) && names_currency(&with_currencies(text))
}

fn operator_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[+\-*/^%()]").unwrap())
}
fn conversion_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `(?i)`, as the JS gate carried: `5 KM IN MILES` is a conversion.
    RE.get_or_init(|| Regex::new(r"(?i)\b(to|in|into|as|para|pra)\b").unwrap())
}
fn money_preposition_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(em|en)\b").unwrap())
}

/// The whole call: what qalc is asked and how the answer is written are one
/// decision. -m bounds a pathological expression.
fn command(text: &str) -> Vec<String> {
    let expression = for_qalc(text);
    let mut argv = vec![
        "qalc".to_string(),
        "-t".to_string(),
        "-m".to_string(),
        "200".to_string(),
        "-set".to_string(),
        format!("exp {EXPONENT_FROM}"),
        "-set".to_string(),
        format!("precision {}", precision_for(&expression)),
    ];
    if is_money(&expression) {
        argv.extend(["-set".into(), "maxdeci 2".into()]);
    }
    if Regex::new(r"(?i)\bto\b")
        .unwrap()
        .is_match(&expression)
        .unwrap_or(false)
    {
        argv.extend(["-set".into(), "conv 0".into()]);
    }
    argv.push("--".into());
    argv.push(expression);
    argv
}

// ------------------------------------------------------------- the provider

impl NativeExt for Calc {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move {
            let expression = ctx.arg.clone();
            // Scoped (`=`/`calc:`) skips the gate: you asked, so you get qalc's
            // reading whatever it is. Unscoped, the gate decides.
            if ctx.query.scope.is_empty() && !looks_like_math(&expression) {
                return NativeOutcome::Empty;
            }
            if expression.is_empty() {
                return NativeOutcome::Empty;
            }

            // The placeholder first: Enter must not fall through to an app
            // while qalc is thinking.
            let _ = progress.send(vec![json!({
                "id": expression,
                "title": expression,
                "subtitle": "calculating",
                "accessory": "",
                "pending": true,
                "view": "hero",
            })]);

            let argv = command(&expression);
            // `-m 200` already asks qalc to bound itself; the outer timeout is
            // for the spawn that never comes back at all, and kill_on_drop
            // makes the aborted run take the child with it.
            let mut cmd = tokio::process::Command::new(&argv[0]);
            cmd.args(&argv[1..])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            let output =
                tokio::time::timeout(std::time::Duration::from_secs(4), cmd.output()).await;
            let Ok(Ok(output)) = output else {
                return NativeOutcome::Empty;
            };
            let stdout = String::from_utf8_lossy(&output.stdout);

            let Some(answer) = parse(&expression, &stdout) else {
                return NativeOutcome::Empty;
            };

            // Copying the answer and remembering it are one gesture. `calc:`
            // reads the file this writes, and its `when` is a test that the
            // file has anything in it.
            let record = format!(
                "oxy-calc-history record {} {}",
                crate::support::quote::quote(&expression),
                crate::support::quote::quote(&answer)
            );
            let detail = reading(&expression);
            let mut row = json!({
                "id": expression,
                "title": answer,
                "subtitle": expression,
                "detail": detail,
                "accessory": "Copy",
                "exec": record,
                "view": "hero",
                "score": 90000,
                "actions": [
                    { "title": "Copy Result", "shortcut": "↵", "exec": record },
                    { "title": "Copy Expression", "exec": format!(
                        "printf %s {} | wl-copy",
                        crate::support::quote::quote(&expression)) },
                ],
            });
            if detail.is_empty() {
                row.as_object_mut().unwrap().remove("detail");
            }
            NativeOutcome::Rows(vec![row])
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate() {
        assert!(looks_like_math("2+2"));
        assert!(looks_like_math("5 km to miles"));
        assert!(!looks_like_math("firefox"));
        assert!(!looks_like_math("5"));
    }
}
