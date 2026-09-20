//! What an extension file means: `Extension` is the normalized definition a
//! JSON file parses into, `Setting` the shape of a `settings:` field, and
//! `def_stamp` the fingerprint a worker gates a cached answer on.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

use crate::model::row::Action;
use crate::support::rank;

#[derive(Debug, Clone)]
pub struct Setting {
    pub key: String,
    pub label: String,
    pub value: String,
    pub placeholder: String,
    pub secret: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Extension {
    pub id: String,
    pub title: String,
    pub keyword: String,
    pub aliases: Vec<String>,
    /// Extra filter names parsed out of the query for this extension.
    pub filters: Vec<String>,
    /// The command that answers, with `{query}`/`{filter}` placeholders.
    pub search: String,
    /// A shell test run once at load; a nonzero exit hides the keyword.
    pub when: String,
    pub glyph: String,
    pub subtitle: String,
    pub min_chars: usize,
    pub debounce_ms: u64,
    pub timeout_ms: u64,
    pub max_rows: usize,
    pub tier: u32,
    /// The layout this extension's results want; a row may override it.
    pub view: String,
    /// Answer unscoped queries too — opt in, deliberately.
    pub always: bool,
    pub cache_ms: u64,
    pub refresh_ms: u64,
    /// A unix socket to ask instead of running a command.
    pub socket: String,
    /// A provider compiled into the daemon answering instead of `search`.
    pub native: String,
    /// `/` commands the extension itself owns.
    pub actions: Vec<Action>,
    /// One colour for this extension's rows.
    pub accent: String,
    /// Fields `settings:` asks for.
    pub settings: Vec<Setting>,
    /// What `oxy test` types at this extension.
    pub test_query: String,
    /// One of the launcher's own — the engines synthesize these, so `?` can
    /// list them under "Built In" rather than beside the files on disk.
    pub builtin: bool,
    pub source: PathBuf,
}

#[derive(Deserialize)]
struct Raw {
    id: Option<Value>,
    title: Option<Value>,
    keyword: Option<Value>,
    aliases: Option<Vec<String>>,
    filters: Option<Vec<String>>,
    search: Option<Value>,
    when: Option<Value>,
    glyph: Option<Value>,
    subtitle: Option<Value>,
    #[serde(rename = "minChars")]
    min_chars: Option<Value>,
    #[serde(rename = "debounceMs")]
    debounce_ms: Option<Value>,
    #[serde(rename = "timeoutMs")]
    timeout_ms: Option<Value>,
    #[serde(rename = "maxRows")]
    max_rows: Option<Value>,
    tier: Option<Value>,
    view: Option<Value>,
    always: Option<bool>,
    #[serde(rename = "cacheMs")]
    cache_ms: Option<Value>,
    #[serde(rename = "refreshMs")]
    refresh_ms: Option<Value>,
    socket: Option<Value>,
    native: Option<Value>,
    actions: Option<Vec<Action>>,
    accent: Option<Value>,
    settings: Option<Vec<Setting2>>,
    #[serde(rename = "testQuery")]
    test_query: Option<Value>,
}

#[derive(Deserialize)]
struct Setting2 {
    key: Option<String>,
    label: Option<String>,
    value: Option<Value>,
    placeholder: Option<String>,
    secret: Option<bool>,
}

fn s(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(x)) => x.trim().to_string(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// The default is the value the field documented, not zero: a hand-edited
/// NaN used to travel as one and fail silently.
fn num(v: Option<&Value>, fallback: f64) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(fallback),
        Some(Value::String(x)) if !x.is_empty() => x.trim().parse::<f64>().unwrap_or(fallback),
        _ => fallback,
    }
}

impl Extension {
    /// `None` for a file that cannot answer anything: no id, or neither a
    /// command, a socket nor a native name. Dropped rather than sitting in the
    /// keyword list as a keyword that does nothing.
    pub fn normalize(raw: &Value, source: PathBuf) -> Option<Extension> {
        let raw: Raw = serde_json::from_value(raw.clone()).ok()?;
        let id = s(raw.id.as_ref());
        if id.is_empty() {
            return None;
        }
        let search = s(raw.search.as_ref());
        let socket = s(raw.socket.as_ref());
        let native = s(raw.native.as_ref());
        if search.is_empty() && socket.is_empty() && native.is_empty() {
            return None;
        }

        // A socket path is the one field an extension cannot write correctly
        // without knowing whose machine it is on, so `~` is expanded here.
        let socket = if let Some(rest) = socket.strip_prefix("~/") {
            dirs_home().join(rest).to_string_lossy().into_owned()
        } else {
            socket
        };

        let title = {
            let t = s(raw.title.as_ref());
            if t.is_empty() { id.clone() } else { t }
        };
        let keyword = {
            let k = s(raw.keyword.as_ref());
            if k.is_empty() { id.clone() } else { k }
        }
        .to_lowercase();

        Some(Extension {
            subtitle: {
                let sub = s(raw.subtitle.as_ref());
                if sub.is_empty() { title.clone() } else { sub }
            },
            id,
            title,
            keyword,
            aliases: raw
                .aliases
                .unwrap_or_default()
                .iter()
                .map(|a| a.to_lowercase())
                .collect(),
            filters: raw
                .filters
                .unwrap_or_default()
                .iter()
                .map(|f| f.to_lowercase())
                .collect(),
            search,
            when: s(raw.when.as_ref()),
            glyph: s(raw.glyph.as_ref()),
            min_chars: num(raw.min_chars.as_ref(), 1.0).max(0.0) as usize,
            debounce_ms: num(raw.debounce_ms.as_ref(), 200.0).max(0.0) as u64,
            timeout_ms: num(raw.timeout_ms.as_ref(), 4000.0).max(0.0) as u64,
            max_rows: num(raw.max_rows.as_ref(), 8.0).max(0.0) as usize,
            tier: rank::tier(&{
                let t = s(raw.tier.as_ref());
                if t.is_empty() {
                    "substring".to_string()
                } else {
                    t
                }
            }),
            view: {
                let v = s(raw.view.as_ref());
                if v.is_empty() { "list".to_string() } else { v }
            },
            always: raw.always == Some(true),
            cache_ms: num(raw.cache_ms.as_ref(), 0.0).max(0.0) as u64,
            refresh_ms: num(raw.refresh_ms.as_ref(), 0.0).max(0.0) as u64,
            socket,
            native,
            actions: raw.actions.unwrap_or_default(),
            accent: s(raw.accent.as_ref()),
            settings: raw
                .settings
                .unwrap_or_default()
                .into_iter()
                .map(|x| Setting {
                    key: x.key.unwrap_or_default(),
                    label: x.label.unwrap_or_default(),
                    value: x
                        .value
                        .map(|v| match v {
                            Value::String(s) => s,
                            other => other.to_string(),
                        })
                        .unwrap_or_default(),
                    placeholder: x.placeholder.unwrap_or_default(),
                    secret: x.secret == Some(true),
                })
                .collect(),
            test_query: s(raw.test_query.as_ref()),
            builtin: false,
            source,
        })
    }

    /// Every keyword and alias this extension answers to — what the query
    /// parser knows as a filter name.
    pub fn keywords(&self) -> Vec<String> {
        let mut out = vec![self.keyword.clone()];
        out.extend(self.aliases.iter().cloned());
        out.extend(self.filters.iter().cloned());
        out
    }
}

fn dirs_home() -> PathBuf {
    crate::settings::paths::home()
}

/// A fingerprint of the whole definition. `cache::ext_stamp` covers only the
/// fields `to_row` reads; the worker also gates on `when`, `search`,
/// `socket`, `debounce_ms` and friends, and a field added later would have
/// to be remembered by hand. The `Debug` text hashes every field including
/// future ones — it is a fingerprint, never parsed back.
pub fn def_stamp(ext: &Extension) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write(format!("{ext:?}").as_bytes());
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_minimal() {
        let ext = Extension::normalize(
            &json!({"id":"x","search":"echo hi"}),
            PathBuf::from("x.json"),
        )
        .unwrap();
        assert_eq!(ext.keyword, "x");
        assert_eq!(ext.tier, rank::TIER_SUBSTRING);
        assert_eq!(ext.view, "list");
        assert_eq!(ext.debounce_ms, 200);
    }

    #[test]
    fn normalize_drops_answerless() {
        assert!(Extension::normalize(&json!({"id":"x"}), PathBuf::new()).is_none());
        assert!(Extension::normalize(&json!({"search":"echo"}), PathBuf::new()).is_none());
    }

    #[test]
    fn native_satisfies_answerable() {
        let ext =
            Extension::normalize(&json!({"id":"sys","native":"sys"}), PathBuf::new()).unwrap();
        assert_eq!(ext.native, "sys");
    }
}
