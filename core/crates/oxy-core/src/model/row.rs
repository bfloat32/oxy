//! A row is the unit everything answers in. This is the shape
//! `Extensions.toRow` produced, so the views on the other end of the socket
//! read the same fields they always did.
//!
//! Anything a script or native provider emits that is not named here is
//! carried through in `extra` — an extension can experiment without this file
//! growing a field for every view.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A choice on a row: Ctrl+K lists them, Shift+Enter runs the second.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Action {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub glyph: String,
    #[serde(default)]
    pub exec: String,
    /// A built-in the daemon performs itself rather than a command to run.
    #[serde(default)]
    pub effect: String,
    /// Asked in the box before the action fires.
    #[serde(default)]
    pub confirm: String,
    /// Drawn on the row, e.g. "↵".
    #[serde(default)]
    pub shortcut: String,
    /// Running it does not dismiss the launcher.
    #[serde(default, rename = "keepOpen")]
    pub keep_open: bool,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    /// Stable identity across rebuilds: `ext:<provider>:<id>`. Selection
    /// follows it, never a position.
    pub key: String,
    #[serde(rename = "providerId")]
    pub provider_id: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub accessory: String,
    /// An icon name or path. (`icon` on the wire.)
    #[serde(default, rename = "iconSource")]
    pub icon_source: String,
    /// A glyph drawn where there is no icon. (`glyph` on the wire.)
    #[serde(default, rename = "iconGlyph")]
    pub icon_glyph: String,
    /// A picture row: album cover, file thumbnail. (`art` on the wire.)
    #[serde(default)]
    pub art: String,
    #[serde(default)]
    pub view: String,
    /// What Ctrl+Enter asks about this row.
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub mono: bool,
    /// 0..1, drawn as a bar by the dashboard view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,

    // A month, drawn as a grid rather than as `cal` output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub month: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub today: Option<i64>,
    #[serde(default, rename = "weekStart", skip_serializing_if = "Option::is_none")]
    pub week_start: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marks: Option<Vec<i64>>,

    // A player: what is playing, how far in, and the calls to change it.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub player: String,
    #[serde(
        default,
        rename = "lengthSeconds",
        skip_serializing_if = "Option::is_none"
    )]
    pub length_seconds: Option<f64>,
    #[serde(default)]
    pub seek: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controls: Option<Value>,
    #[serde(default)]
    pub shuffle: bool,
    #[serde(default, rename = "loop")]
    pub loop_: String,
    #[serde(default, rename = "canNext")]
    pub can_next: bool,
    #[serde(default, rename = "canPrev")]
    pub can_prev: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<Action>>,

    // A slider: a number the user drags, written back with `setExec`.
    #[serde(default)]
    pub value: f64,
    #[serde(default)]
    pub min: f64,
    #[serde(default)]
    pub max: f64,
    #[serde(default)]
    pub step: f64,
    #[serde(default, rename = "setExec")]
    pub set_exec: String,

    #[serde(default)]
    pub tier: u32,
    /// The provider's own order inside its tier (0..99999).
    #[serde(default)]
    pub local: i64,
    /// `tier * 100000 + local`, plus frecency and pin lifts.
    #[serde(default)]
    pub score: i64,
    /// What Enter runs: a detached command, or empty for daemon-typed rows.
    #[serde(default)]
    pub exec: String,
    /// Text typed into the box on Enter instead of running anything — a
    /// keyword from `?`, or a query you ran before.
    #[serde(default, rename = "fill", skip_serializing_if = "String::is_empty")]
    pub fill: String,
    /// A placeholder row shown while the real answer is still being computed.
    #[serde(default)]
    pub pending: bool,
    #[serde(default)]
    pub pinned: bool,
    /// Run as the selection lands on the row (theme preview); paired with
    /// `revert_exec`.
    #[serde(
        default,
        rename = "previewExec",
        skip_serializing_if = "String::is_empty"
    )]
    pub preview_exec: String,
    #[serde(
        default,
        rename = "revertExec",
        skip_serializing_if = "String::is_empty"
    )]
    pub revert_exec: String,

    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One provider's answer set, shared end to end: the worker's Rows message,
/// the cache entry, the engine's bucket, and the merge input can all be the
/// same allocation — handing one along is a refcount bump, never a copy.
pub type SharedRows = std::sync::Arc<Vec<std::sync::Arc<Row>>>;

impl Row {
    /// The row a provider starts from: identity and nothing else.
    pub fn new(key: impl Into<String>, provider_id: impl Into<String>) -> Row {
        Row {
            key: key.into(),
            provider_id: provider_id.into(),
            group: String::new(),
            title: String::new(),
            subtitle: String::new(),
            detail: String::new(),
            accessory: String::new(),
            icon_source: String::new(),
            icon_glyph: String::new(),
            art: String::new(),
            view: String::new(),
            preview: String::new(),
            mono: false,
            progress: None,
            year: None,
            month: None,
            today: None,
            week_start: None,
            marks: None,
            status: String::new(),
            player: String::new(),
            length_seconds: None,
            seek: String::new(),
            controls: None,
            shuffle: false,
            loop_: "None".to_string(),
            can_next: true,
            can_prev: true,
            actions: None,
            value: 0.0,
            min: 0.0,
            max: 100.0,
            step: 1.0,
            set_exec: String::new(),
            tier: 0,
            local: 0,
            score: 0,
            exec: String::new(),
            fill: String::new(),
            pending: false,
            pinned: false,
            preview_exec: String::new(),
            revert_exec: String::new(),
            extra: Map::new(),
        }
    }
}

/// Names the launcher owns. A script that sets one is naming something it does
/// not get to decide, so they are dropped from the passthrough copy.
const RESERVED: &[&str] = &[
    "key",
    "providerId",
    "tier",
    "local",
    "score",
    "run",
    "pending",
    "icon",
    "glyph",
];

/// Pull `key` out of the object as an owned `String` — moving the value out
/// of the map instead of cloning it. Non-strings read as empty, matching the
/// old `as_str().unwrap_or("")` behaviour.
fn take_str(obj: &mut Map<String, Value>, key: &str) -> String {
    match obj.remove(key) {
        Some(Value::String(s)) => s,
        _ => String::new(),
    }
}

/// Build a launcher row from a provider's raw JSON row — the `toRow` port.
///
/// A script's own `score` orders its rows against each other, clamped into
/// `local`. It never crosses tiers.
///
/// Borrowed entry point — used where the raw rows must stay intact (the
/// cache serves the same `Value`s again on the next hit). It clones once and
/// delegates to the consuming version, so there is exactly one conversion to
/// keep honest.
pub fn to_row(ext: &crate::registry::Extension, raw: &Value, index: usize) -> Option<Row> {
    match raw {
        Value::Object(obj) => to_row_inner(ext, obj.clone(), index),
        _ => None,
    }
}

/// The consuming entry point: the provider's map is taken apart field by
/// field — every string moves into the row un-cloned, and what is left over
/// *is* the passthrough map. Zero field copies on the fresh path.
pub fn to_row_owned(ext: &crate::registry::Extension, raw: Value, index: usize) -> Option<Row> {
    match raw {
        Value::Object(obj) => to_row_inner(ext, obj, index),
        _ => None,
    }
}

fn to_row_inner(
    ext: &crate::registry::Extension,
    mut obj: Map<String, Value>,
    index: usize,
) -> Option<Row> {
    let id_field = obj.remove("id");
    let title = take_str(&mut obj, "title");
    if title.is_empty() {
        return None;
    }
    let id = match id_field {
        Some(Value::String(s)) => s,
        Some(other) => other.to_string(),
        // No id field: the title names the row (non-empty — checked above).
        None => title.clone(),
    };

    let local = obj
        .remove("score")
        .and_then(|s| s.as_f64())
        .map(|s| s.clamp(0.0, 99999.0) as i64)
        .unwrap_or_else(|| (90000 - index as i64 * 1000).max(0));

    let num = |obj: &mut Map<String, Value>, key: &str| obj.remove(key).and_then(|v| v.as_f64());
    let int = |obj: &mut Map<String, Value>, key: &str| obj.remove(key).and_then(|v| v.as_i64());
    let yes = |obj: &mut Map<String, Value>, key: &str| {
        obj.remove(key).and_then(|v| v.as_bool()).unwrap_or(false)
    };

    let mut row = Row::new(format!("ext:{}:{id}", ext.id), &ext.id);
    row.group = {
        let g = take_str(&mut obj, "group");
        if g.is_empty() { ext.title.clone() } else { g }
    };
    row.title = title;
    row.subtitle = match obj.remove("subtitle") {
        Some(Value::String(s)) => s,
        _ => ext.subtitle.clone(),
    };
    row.detail = take_str(&mut obj, "detail");
    row.accessory = take_str(&mut obj, "accessory");
    row.icon_source = take_str(&mut obj, "icon");
    row.icon_glyph = {
        let g = take_str(&mut obj, "glyph");
        if g.is_empty() { ext.glyph.clone() } else { g }
    };
    row.art = take_str(&mut obj, "art");
    // Only the first row's view is read, so a script puts the row it wants to
    // set the layout first and the rest follow it.
    row.view = {
        let v = take_str(&mut obj, "view");
        if v.is_empty() { ext.view.clone() } else { v }
    };
    row.preview = take_str(&mut obj, "preview");
    row.mono = yes(&mut obj, "mono");
    row.progress = num(&mut obj, "progress");
    row.year = int(&mut obj, "year");
    row.month = int(&mut obj, "month");
    row.today = int(&mut obj, "today");
    row.week_start = int(&mut obj, "weekStart");
    row.marks = obj.remove("marks").and_then(|v| match v {
        Value::Array(a) => Some(a.iter().filter_map(|n| n.as_i64()).collect()),
        _ => None,
    });
    row.status = take_str(&mut obj, "status");
    row.player = take_str(&mut obj, "player");
    row.length_seconds = num(&mut obj, "lengthSeconds");
    row.seek = take_str(&mut obj, "seek");
    row.controls = obj.remove("controls").filter(|v| !v.is_null());
    row.shuffle = yes(&mut obj, "shuffle");
    row.loop_ = {
        let l = take_str(&mut obj, "loop");
        if l.is_empty() { "None".into() } else { l }
    };
    row.can_next = obj
        .remove("canNext")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    row.can_prev = obj
        .remove("canPrev")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    // Owned, not cloned: the whole actions array moves without a deep copy.
    row.actions = obj
        .remove("actions")
        .and_then(|v| serde_json::from_value::<Vec<Action>>(v).ok());
    row.value = num(&mut obj, "value").unwrap_or(0.0);
    row.min = num(&mut obj, "min").unwrap_or(0.0);
    row.max = num(&mut obj, "max").unwrap_or(100.0);
    row.step = num(&mut obj, "step").unwrap_or(1.0);
    row.set_exec = take_str(&mut obj, "setExec");
    row.tier = ext.tier;
    row.local = local;
    row.exec = take_str(&mut obj, "exec");
    row.fill = take_str(&mut obj, "fill");
    row.preview_exec = take_str(&mut obj, "previewExec");
    row.revert_exec = take_str(&mut obj, "revertExec");

    // Anything the launcher has not already named is carried across untouched.
    // Every typed field was removed above; the launcher-owned leftovers go
    // too, and what remains *is* the extra map — moved, not copied.
    for key in RESERVED {
        obj.remove(*key);
    }
    row.extra = obj;

    Some(row)
}

/// Accepts a JSON array, or one JSON object per line — a port of
/// `Extensions.parseRows`. Anything before the first `[`/`{` is not ours: a
/// shim's chatter line must not blank a perfect answer.
pub fn parse_rows(text: &str) -> Vec<Value> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let start = trimmed.find(['[', '{']).unwrap_or(usize::MAX);
    let trimmed = if start == usize::MAX {
        ""
    } else {
        &trimmed[start..]
    };

    if let Ok(whole) = serde_json::from_str::<Value>(trimmed) {
        match whole {
            Value::Array(a) => return a,
            Value::Object(_) => return vec![whole],
            _ => {}
        }
    }

    let mut rows = Vec::new();
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // One malformed line should not lose the rest of a long answer.
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            rows.push(v);
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_array() {
        let rows = parse_rows(r#"[{"title":"a"},{"title":"b"}]"#);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn parses_lines_and_skips_chatter() {
        let rows = parse_rows("mise: tools loaded\n{\"title\":\"a\"}\nnot json\n{\"title\":\"b\"}");
        assert_eq!(rows.len(), 2);
    }

    fn test_ext(id: &str) -> crate::registry::Extension {
        crate::registry::Extension {
            id: id.into(),
            title: "Files".into(),
            subtitle: "File".into(),
            glyph: "F".into(),
            tier: 4,
            view: "files".into(),
            ..Default::default()
        }
    }

    #[test]
    fn to_row_copies_unknown_fields() {
        let raw = json!({"id":"x","title":"T","score":123,"custom":{"a":1},"exec":"true"});
        let row = to_row(&test_ext("file"), &raw, 0).unwrap();
        assert_eq!(row.key, "ext:file:x");
        assert_eq!(row.local, 123);
        assert_eq!(row.extra["custom"], json!({"a":1}));
        assert!(row.extra.get("exec").is_none());
    }

    #[test]
    fn empty_title_is_dropped() {
        let raw = json!({"id":"x"});
        assert!(to_row(&test_ext("e"), &raw, 0).is_none());
    }
}
