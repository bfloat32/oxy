use serde_json::Value;

/// A field is absent when missing, "", [] or {}. 0 and false are real.
pub(crate) fn present(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(_) => true,
    }
}

/// `matches` patterns were written against Python's `str()`: a bool reads
/// `True`, a list `['a', 'b']`, a dict `{'k': 'v'}` — so the row's value is
/// rendered the same way before the regex sees it.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Null => "None".into(),
        Value::Number(n) => n.to_string(),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr_str(s),
        other => py_str(other),
    }
}

fn py_repr_str(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// One case's assertions against the rows its extension answered — a port of
/// `tests/cases.py`'s `check_case`. Row assertions only check when the row
/// exists; `minRows` is how a case demands one.
pub(crate) fn check_case(case: &Value, rows: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    let n = rows.len();
    if let Some(min) = case.get("minRows").and_then(|v| v.as_u64())
        && (n as u64) < min
    {
        out.push(format!("{n} rows, wanted at least {min}"));
        return out;
    }
    if let Some(max) = case.get("maxRows").and_then(|v| v.as_u64())
        && (n as u64) > max
    {
        out.push(format!("{n} rows, wanted at most {max}"));
        return out;
    }

    let idx = case.get("row").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let Some(row) = rows.get(idx) else {
        return out;
    };

    if let Some(view) = case.get("view").and_then(|v| v.as_str())
        && row.get("view").and_then(|v| v.as_str()) != Some(view)
    {
        out.push(format!(
            "row {idx} view is {:?}, wanted {view:?}",
            row.get("view")
        ));
        return out;
    }
    for f in case
        .get("fields")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        if let Some(name) = f.as_str()
            && !present(row.get(name))
        {
            out.push(format!("row {idx} {name} is empty"));
        }
    }
    for f in case
        .get("absent")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        if let Some(name) = f.as_str()
            && present(row.get(name))
        {
            out.push(format!(
                "row {idx} {name} is {:?}, wanted absent",
                row.get(name)
            ));
        }
    }
    if let Some(caps) = case.get("atMost").and_then(|v| v.as_object()) {
        for (f, cap) in caps {
            let len = match row.get(f) {
                Some(Value::String(s)) => Some(s.len()),
                Some(Value::Array(a)) => Some(a.len()),
                Some(Value::Object(o)) => Some(o.len()),
                _ => None,
            };
            if let (Some(len), Some(cap)) = (len, cap.as_u64())
                && len as u64 > cap
            {
                out.push(format!("row {idx} {f} has {len}, wanted at most {cap}"));
            }
        }
    }
    if let Some(pats) = case.get("matches").and_then(|v| v.as_object()) {
        for (f, pat) in pats {
            let Some(pat) = pat.as_str() else { continue };
            let text = py_str(row.get(f).unwrap_or(&Value::Null));
            let matched = fancy_regex::Regex::new(pat)
                .ok()
                .and_then(|re| re.is_match(&text).ok())
                .unwrap_or(false);
            if !matched {
                let short: String = text.chars().take(120).collect();
                out.push(format!("row {idx} {f} is {short:?}, wanted /{pat}/"));
            }
        }
    }
    out
}
