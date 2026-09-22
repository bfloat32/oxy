//! The jq shim every emitter shares — `.k` indexing that dies on
//! scalars, `//`-style alternation, `@sh` quoting, and the small
//! derivations (`ago`, `mark`, `revword`, `prstate`) the rows draw from.
//! `None` is what jq called death: silence, printed as no row.

pub(super) use serde_json::{Map, Value, json};

pub(super) use crate::provider::native::util::short_age;
pub(super) use crate::support::quote::quote;

pub(super) const BROWSER: &str = "omarchy-launch-browser";
pub(super) const TUI: &str = "omarchy-launch-tui --app-id=org.omarchy.git";

pub(super) static NULL: Value = Value::Null;

// ---------------------------------------------------------------- jq shim

/// `.k` — a missing key is null, null has no fields, and a scalar parent
/// is the "cannot index" error that killed the emit.
pub(super) fn at<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => Some(m.get(k).unwrap_or(&NULL)),
        Value::Null => Some(&NULL),
        _ => None,
    }
}

/// `.[i]` — out of bounds is null; a scalar parent is the same death.
pub(super) fn ati(v: &Value, i: usize) -> Option<&Value> {
    match v {
        Value::Array(a) => Some(a.get(i).unwrap_or(&NULL)),
        Value::Null => Some(&NULL),
        _ => None,
    }
}

/// `x // d` — null, false and absent all fall through to the default.
pub(super) fn alt<'a>(v: Option<&'a Value>, d: &'a Value) -> &'a Value {
    match v {
        Some(Value::Null) | Some(Value::Bool(false)) | None => d,
        Some(o) => o,
    }
}

/// A `// ""` read *concatenated into* a string: a surviving non-string is
/// jq's "cannot add" death.
pub(super) fn so(v: Option<&Value>) -> Option<String> {
    match v.unwrap_or(&NULL) {
        Value::Null | Value::Bool(false) => Some(String::new()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// A bare `.field` in a `+` chain: null is the identity (`null + "x"` is
/// `"x"`), anything but a string or null is the death.
pub(super) fn sc(v: Option<&Value>) -> Option<String> {
    match v.unwrap_or(&NULL) {
        Value::Null => Some(String::new()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// `x // ""` where the result is *emitted* rather than concatenated:
/// null and false become "", anything else is kept verbatim.
pub(super) fn emit_or_empty(v: Option<&Value>) -> Value {
    match v.unwrap_or(&NULL) {
        Value::Null | Value::Bool(false) => Value::String(String::new()),
        other => other.clone(),
    }
}

/// `[(x // [])[].name]` — the label-name list. A scalar node is the
/// index death; a missing name reads null and `join` prints it empty.
pub(super) fn names(v: Option<&Value>) -> Option<Vec<Value>> {
    list(alt(v, &NULL))?
        .iter()
        .map(|n| at(n, "name").cloned())
        .collect()
}

/// `(x // [])[]` — null and false iterate as empty; a scalar is the death.
pub(super) fn list(v: &Value) -> Option<&[Value]> {
    match v {
        Value::Null | Value::Bool(false) => Some(&[]),
        Value::Array(a) => Some(a),
        _ => None,
    }
}

/// jq `if x` — only null and false are falsy; `0`, `""` and `[]` are true.
pub(super) fn truthy(v: &Value) -> bool {
    !matches!(v, Value::Null | Value::Bool(false))
}

/// `(x // 0) > 0` under jq's total order — strings and arrays are all
/// "over" any number, so a surprise type reads true rather than dying.
pub(super) fn over_zero(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(_) => false,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) > 0.0,
        _ => true,
    }
}

/// jq `tostring`: null → "null", bools → "true"/"false", numbers their
/// shortest form, strings themselves, arrays/objects compact JSON.
pub(super) fn tostring(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                n.to_string()
            }
        }
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// jq `join`: nulls render empty, scalars stringify, and a nested
/// array/object is the death.
pub(super) fn join(parts: &[Value], sep: &str) -> Option<String> {
    let mut out = String::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push_str(sep);
        }
        match p {
            Value::Null => {}
            Value::Bool(_) | Value::Number(_) => out.push_str(&tostring(p)),
            Value::String(s) => out.push_str(s),
            _ => return None,
        }
    }
    Some(out)
}

/// `map(select(. != "")) | join(sep)` — `!=` keeps every non-string, so a
/// kept array/object still dies in `join`.
pub(super) fn join_ne(parts: Vec<Value>, sep: &str) -> Option<String> {
    let kept: Vec<Value> = parts
        .into_iter()
        .filter(|p| !matches!(p, Value::String(s) if s.is_empty()))
        .collect();
    join(&kept, sep)
}

/// `fromdateiso8601?` — a stamp that does not parse is suppressed to null.
pub(super) fn parse_iso(s: &str) -> Option<i64> {
    s.parse::<jiff::Timestamp>().ok().map(|t| t.as_second())
}

/// `ago($iso; $now)`: "now" under a minute, then m/h/d/mo/y. An
/// unreadable or absent stamp is "" — the `?` on `fromdateiso8601`.
pub(super) fn ago(v: &Value, now: i64) -> String {
    let Some(s) = v.as_str() else {
        return String::new();
    };
    if s.is_empty() {
        return String::new();
    }
    match parse_iso(s) {
        Some(t) => short_age(now - t),
        None => String::new(),
    }
}

/// `mark` — the check-state glyph. `ascii_upcase` on a non-string is the
/// death, so this is `Option`.
pub(super) fn mark(v: &Value) -> Option<&'static str> {
    let s = match v {
        Value::Null | Value::Bool(false) => "",
        Value::String(s) => s.as_str(),
        _ => return None,
    };
    Some(match s.to_ascii_uppercase().as_str() {
        "SUCCESS" => "✓",
        "FAILURE" | "ERROR" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED" => "✗",
        "CANCELLED" | "STALE" => "⊘",
        "NEUTRAL" | "SKIPPED" => "–",
        "" | "NULL" => "",
        _ => "●",
    })
}

/// `revword` — the review decision as a word, "" when there is nothing
/// worth saying. `==` never dies, so neither does this.
pub(super) fn revword(v: &Value) -> &'static str {
    match v.as_str() {
        Some("APPROVED") => "approved",
        Some("CHANGES_REQUESTED") => "changes requested",
        Some("REVIEW_REQUIRED") => "review needed",
        _ => "",
    }
}

/// `prstate` — `.commits.nodes[0].commit.statusCheckRollup.state // ""`.
pub(super) fn prstate(p: &Value) -> Option<Value> {
    Some(
        alt(
            at(
                at(
                    at(ati(at(at(p, "commits")?, "nodes")?, 0)?, "commit")?,
                    "statusCheckRollup",
                )?,
                "state",
            ),
            &NULL,
        )
        .clone(),
    )
}

/// jq `@sh` — strings single-quoted, scalars bare, arrays space-joined;
/// an object is the death.
pub(super) fn jqsh(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(quote(s)),
        Value::Null => Some("null".to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(_) => Some(tostring(v)),
        Value::Array(a) => {
            let mut out = String::new();
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                out.push_str(&jqsh(e)?);
            }
            Some(out)
        }
        Value::Object(_) => None,
    }
}

/// `openurl` — `'omarchy-launch-browser' ` + `@sh`. The url field is
/// passed raw, because `@sh` does not die on a missing one.
pub(super) fn openurl(u: &Value) -> Option<String> {
    jqsh(u).map(|q| format!("'{BROWSER}' {q}"))
}

/// `copyurl` — `printf %s ` + `@sh` + ` | wl-copy`.
pub(super) fn copyurl(u: &Value) -> Option<String> {
    jqsh(u).map(|q| format!("printf %s {q} | wl-copy"))
}

/// `(mark + " " + ago) | ltrimstr(" ")` — the check mark beside the age.
pub(super) fn accessory(mark: &str, age: String) -> String {
    let s = format!("{mark} {age}");
    s.strip_prefix(' ').map(str::to_string).unwrap_or(s)
}
