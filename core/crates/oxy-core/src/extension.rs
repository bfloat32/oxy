//! An extension is one JSON file in `~/.config/omarchy/oxy/extensions/`. It
//! names a keyword and something that answers for it. A port of
//! `Extensions.normalize` plus the loader `Launcher.applyExtensions` ran.
//!
//! On this branch an extension may also name a `"native"` implementation — a
//! provider compiled into the daemon — and may carry both `native` and
//! `search`, in which case the native provider answers first and may decline
//! to the command.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::rank;
use crate::row::Action;
use crate::shellquote;

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
    crate::dirs::home()
}

/// What a malformed file logged. A bad extension failing silently is the one
/// bug a user cannot see.
pub struct LoadReport {
    pub extensions: Vec<Extension>,
    /// (file, why) for every file that could not be loaded.
    pub bad: Vec<(PathBuf, String)>,
}

/// Read every `*.json` in the dir except `*.cases.json`, normalize each.
/// `enabled` is the `extensions` map from settings: absent means on.
pub fn load_dir(dir: &Path, enabled: &serde_json::Map<String, Value>) -> LoadReport {
    let mut report = LoadReport {
        extensions: Vec::new(),
        bad: Vec::new(),
    };

    let mut files: Vec<PathBuf> = Vec::new();
    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".json") && !name.ends_with(".cases.json") {
                files.push(path);
            }
        }
    }
    files.sort();

    for path in files {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                report.bad.push((path, format!("read: {e}")));
                continue;
            }
        };
        let raw = match serde_json::from_str::<Value>(&text) {
            Ok(v) => v,
            Err(e) => {
                report.bad.push((path, format!("json: {e}")));
                continue;
            }
        };
        match Extension::normalize(&raw, path.clone()) {
            Some(ext) => {
                // Absent means on. Name one false to silence it.
                let off = enabled.get(&ext.id).and_then(|v| v.as_bool()) == Some(false);
                if !off {
                    report.extensions.push(ext);
                }
            }
            None => report.bad.push((path, "normalize".to_string())),
        }
    }
    report
}

/// `{query}` and `{any-filter}` are replaced by shell-quoted values. Anything
/// unmatched becomes an empty quoted string rather than a literal brace.
pub fn build_command(
    ext: &Extension,
    arg_text: &str,
    filters: &std::collections::BTreeMap<String, String>,
    settings: Option<&serde_json::Map<String, Value>>,
) -> String {
    let re = placeholder_re();
    let command = re
        .replace_all(&ext.search, |caps: &fancy_regex::Captures| {
            let key = caps
                .get(1)
                .map(|m| m.as_str().to_lowercase())
                .unwrap_or_default();
            if key == "query" {
                shellquote::quote(arg_text)
            } else if let Some(v) = filters.get(&key) {
                shellquote::quote(v)
            } else {
                shellquote::quote("")
            }
        })
        .into_owned();

    settings_prefix(settings) + &command
}

fn placeholder_re() -> &'static fancy_regex::Regex {
    static RE: std::sync::OnceLock<fancy_regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        fancy_regex::Regex::new(r"\{([a-z0-9_-]+)\}").expect("placeholder regex compiles")
    })
}

/// What `settings:` collected, handed to the script as environment.
/// Environment rather than arguments: an argument is visible in every process
/// listing on the machine, and a token is the first thing anybody puts here.
fn settings_prefix(settings: Option<&serde_json::Map<String, Value>>) -> String {
    let mut out = String::new();
    let Some(settings) = settings else { return out };
    for (key, value) in settings {
        // Only what could be an environment name, so a hand-edited config
        // cannot inject a second command through a key.
        let valid = key.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid || value.is_null() {
            continue;
        }
        let rendered = match value {
            Value::String(x) => x.clone(),
            other => other.to_string(),
        };
        out.push_str(&format!(
            "OXY_{}={} ",
            key.to_uppercase(),
            shellquote::quote(&rendered)
        ));
    }
    out
}

/// The key an answer is cached and refreshed under: the command itself for a
/// command extension, the spelled-out question for a socket or native one.
pub fn cache_key(
    ext: &Extension,
    command: &str,
    arg_text: &str,
    filters: &std::collections::BTreeMap<String, String>,
) -> String {
    if !command.is_empty() {
        return command.to_string();
    }
    let filters = serde_json::to_string(filters).unwrap_or_default();
    format!("ask\0{}\0{}\0{}", ext.socket, arg_text, filters)
}

/// The keyword set the parser validates against: every keyword, alias and
/// declared filter of every extension, plus the built-in scopes.
pub fn known_keywords(extensions: &[Extension], extra: &HashSet<String>) -> HashSet<String> {
    let mut out = extra.clone();
    for ext in extensions {
        out.extend(ext.keywords());
    }
    out
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

    #[test]
    fn build_command_substitutes() {
        let ext = Extension::normalize(
            &json!({"id":"file","search":"find {query} {format}","filters":["format"]}),
            PathBuf::new(),
        )
        .unwrap();
        let mut filters = std::collections::BTreeMap::new();
        filters.insert("format".to_string(), "pdf".to_string());
        let cmd = build_command(&ext, "report's", &filters, None);
        assert_eq!(cmd, "find 'report'\\''s' 'pdf'");
    }
}
