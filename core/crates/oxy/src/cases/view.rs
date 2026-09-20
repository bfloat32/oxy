use serde_json::{Value, json};

/// The services some cases describe — unreachable here means skip, the same
/// preflight cases.py runs.
pub(crate) async fn case_preflight(id: &str) -> bool {
    let probe = match id {
        "define" => {
            "curl -sf --max-time 6 -o /dev/null \
             https://api.dictionaryapi.dev/api/v2/entries/en/ping"
        }
        "issue" | "pr" => "gh auth status",
        // Names like saopaulo resolve through the IANA list in tzdata, not
        // the shipped label table.
        "timezone" => "timedatectl list-timezones >/dev/null 2>&1 || test -d /usr/share/zoneinfo",
        _ => return true,
    };
    oxy_core::provider::process::check(probe).await
}

/// The row a case asserts on: the serialized wire row, plus the names the
/// fixtures were written against. The wire carries `iconGlyph` and a `key`
/// of `ext:<id>:<row-id>`; the scripts that own the cases say `glyph` and
/// `id` — so those names are mapped back here rather than the cases being
/// rewritten for the wire's spelling.
pub(crate) fn case_view(row: &oxy_core::model::row::Row, ext_id: &str) -> Value {
    let mut v = serde_json::to_value(row).unwrap_or(Value::Null);
    let Some(obj) = v.as_object_mut() else {
        return v;
    };
    let prefix = format!("ext:{ext_id}:");
    if let Some(id) = row.key.strip_prefix(&prefix) {
        obj.insert("id".into(), json!(id));
    }
    if !obj.contains_key("glyph")
        && let Some(g) = obj.get("iconGlyph").cloned()
    {
        obj.insert("glyph".into(), g);
    }
    if !obj.contains_key("icon")
        && let Some(i) = obj.get("iconSource").cloned()
    {
        obj.insert("icon".into(), i);
    }
    v
}
