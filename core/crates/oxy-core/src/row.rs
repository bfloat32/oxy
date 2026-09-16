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

/// Build a launcher row from a provider's raw JSON row — the `toRow` port.
///
/// A script's own `score` orders its rows against each other, clamped into
/// `local`. It never crosses tiers.
pub fn to_row(
    ext_id: &str,
    ext_title: &str,
    ext_subtitle: &str,
    ext_glyph: &str,
    ext_tier: u32,
    ext_view: &str,
    ext_max_rows: usize,
    raw: &Value,
    index: usize,
) -> Option<Row> {
    let obj = raw.as_object()?;

    let id = obj
        .get("id")
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| {
            obj.get("title")
                .and_then(|t| t.as_str().map(String::from))
                .unwrap_or_else(|| index.to_string())
        });

    let local = obj
        .get("score")
        .and_then(|s| s.as_f64())
        .map(|s| s.clamp(0.0, 99999.0) as i64)
        .unwrap_or_else(|| (90000 - index as i64 * 1000).max(0));

    let get_str = |name: &str| obj.get(name).and_then(|v| v.as_str()).unwrap_or("");

    let title = get_str("title").to_string();
    if title.is_empty() {
        return None;
    }

    let mut row = Row::new(format!("ext:{ext_id}:{id}"), ext_id);
    row.group = if get_str("group").is_empty() {
        ext_title.to_string()
    } else {
        get_str("group").to_string()
    };
    row.title = title;
    row.subtitle = obj
        .get("subtitle")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| ext_subtitle.to_string());
    row.detail = get_str("detail").into();
    row.accessory = get_str("accessory").into();
    row.icon_source = get_str("icon").into();
    row.icon_glyph = if get_str("glyph").is_empty() {
        ext_glyph.to_string()
    } else {
        get_str("glyph").into()
    };
    row.art = get_str("art").into();
    // Only the first row's view is read, so a script puts the row it wants to
    // set the layout first and the rest follow it.
    row.view = if get_str("view").is_empty() {
        ext_view.to_string()
    } else {
        get_str("view").into()
    };
    row.preview = get_str("preview").into();
    row.mono = obj.get("mono").and_then(|v| v.as_bool()).unwrap_or(false);
    row.progress = obj.get("progress").and_then(|v| v.as_f64());
    row.year = obj.get("year").and_then(|v| v.as_i64());
    row.month = obj.get("month").and_then(|v| v.as_i64());
    row.today = obj.get("today").and_then(|v| v.as_i64());
    row.week_start = obj.get("weekStart").and_then(|v| v.as_i64());
    row.marks = obj.get("marks").and_then(|v| {
        v.as_array()
            .map(|a| a.iter().filter_map(|n| n.as_i64()).collect())
    });
    row.status = get_str("status").into();
    row.player = get_str("player").into();
    row.length_seconds = obj.get("lengthSeconds").and_then(|v| v.as_f64());
    row.seek = get_str("seek").into();
    row.controls = obj.get("controls").cloned();
    row.shuffle = obj
        .get("shuffle")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    row.loop_ = if get_str("loop").is_empty() {
        "None".into()
    } else {
        get_str("loop").into()
    };
    row.can_next = obj.get("canNext").and_then(|v| v.as_bool()).unwrap_or(true);
    row.can_prev = obj.get("canPrev").and_then(|v| v.as_bool()).unwrap_or(true);
    row.actions = obj
        .get("actions")
        .and_then(|v| serde_json::from_value::<Vec<Action>>(v.clone()).ok());
    row.value = obj.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
    row.min = obj.get("min").and_then(|v| v.as_f64()).unwrap_or(0.0);
    row.max = obj.get("max").and_then(|v| v.as_f64()).unwrap_or(100.0);
    row.step = obj.get("step").and_then(|v| v.as_f64()).unwrap_or(1.0);
    row.set_exec = get_str("setExec").into();
    row.tier = ext_tier;
    row.local = local;
    row.exec = get_str("exec").into();
    row.fill = get_str("fill").into();
    row.preview_exec = get_str("previewExec").into();
    row.revert_exec = get_str("revertExec").into();

    // Anything the launcher has not already named is copied across untouched —
    // an extension carries its own fields through for its own use.
    for (field, value) in obj {
        if RESERVED.contains(&field.as_str()) {
            continue;
        }
        // The typed fields were all read above; skip only those names, keep
        // everything else verbatim.
        if matches!(
            field.as_str(),
            "id" | "group"
                | "title"
                | "subtitle"
                | "detail"
                | "accessory"
                | "art"
                | "view"
                | "preview"
                | "mono"
                | "progress"
                | "year"
                | "month"
                | "today"
                | "weekStart"
                | "marks"
                | "status"
                | "player"
                | "lengthSeconds"
                | "seek"
                | "controls"
                | "shuffle"
                | "loop"
                | "canNext"
                | "canPrev"
                | "actions"
                | "value"
                | "min"
                | "max"
                | "step"
                | "setExec"
                | "exec"
                | "fill"
                | "previewExec"
                | "revertExec"
        ) {
            continue;
        }
        row.extra.insert(field.clone(), value.clone());
    }

    let _ = ext_max_rows;
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

    let start = trimmed.find(|c| c == '[' || c == '{').unwrap_or(usize::MAX);
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

    #[test]
    fn to_row_copies_unknown_fields() {
        let raw = json!({"id":"x","title":"T","score":123,"custom":{"a":1},"exec":"true"});
        let row = to_row("file", "Files", "File", "F", 4, "files", 8, &raw, 0).unwrap();
        assert_eq!(row.key, "ext:file:x");
        assert_eq!(row.local, 123);
        assert_eq!(row.extra["custom"], json!({"a":1}));
        assert!(row.extra.get("exec").is_none());
    }

    #[test]
    fn empty_title_is_dropped() {
        let raw = json!({"id":"x"});
        assert!(to_row("e", "E", "E", "", 6, "list", 8, &raw, 0).is_none());
    }
}
