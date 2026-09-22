use serde_json::{Value, json};

/// The services some cases describe — unreachable here means skip, the same
/// preflight cases.py runs.
pub(crate) async fn case_preflight(id: &str) -> bool {
    // Keyed by the extension's *id* — and by its file's name too, because two
    // of them differ (`def` lives in `define.json`, `tz` in `timezone.json`)
    // and this preflight used the file names, so neither ever fired.
    let probe = match id {
        "define" | "def" => {
            "curl -sf --max-time 6 -o /dev/null \
             https://api.dictionaryapi.dev/api/v2/entries/en/ping"
        }
        "issue" | "pr" => "gh auth status",
        // Names like saopaulo resolve through the IANA list in tzdata, not
        // the shipped label table.
        "timezone" | "tz" => {
            "timedatectl list-timezones >/dev/null 2>&1 || test -d /usr/share/zoneinfo"
        }
        // The git family's cases describe throwaway repos that `cases.py`
        // builds under `$HOME/repos`. This runner has no fixture builder yet
        // (§4.7), so without them the honest answer is a skip rather than
        // thirty failures that look like broken ports.
        "git" | "stash" | "branch" => "test -d \"$HOME/repos/oxy-fixture/.git\"",
        "repo" => "test -d \"$HOME/repos/omarchy/.git\"",
        // The agent cases describe the draft a configured agent produces in a
        // Hyprland session; `cases.py` stubs the CLI, and nothing here does.
        "agent" => "command -v hyprctl",
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
    // The scripts emit a bare 0–99999 score; the wire's is
    // `tier * 100000 + local`. The cases were written against the scripts, so
    // the runner hands them the local part — the same number, in the same
    // scale, whichever leg answered. (A native that computes 87999 and a
    // script that prints 87999 are then compared as equals, which is the
    // point.)
    obj.insert("score".into(), json!(row.local));
    v
}
