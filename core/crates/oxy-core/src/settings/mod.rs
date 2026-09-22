//! Everything the user can change, in `~/.config/omarchy/oxy.json` — the same
//! file `Settings.js` read, so one config serves both frontends.

pub mod paths;

mod defaults;

use std::path::Path;

use serde_json::{Map, Value};

pub use defaults::*;

#[derive(Debug, Clone)]
pub struct Engine {
    pub id: String,
    pub title: String,
    pub url: String,
}

/// One `askProviders` entry: a command that takes `{query}` (quoted) and
/// `{model}` (bare), writes its answer to stdout as it goes, and exits.
#[derive(Debug, Clone)]
pub struct AskProvider {
    pub id: String,
    pub title: String,
    pub model: String,
    pub command: String,
    pub when: String,
}

/// The `ask` block: a local model endpoint spoken to directly, in this
/// process, instead of through a CLI. An empty endpoint means the
/// `askProviders` list answers, exactly as it always did.
#[derive(Debug, Clone)]
pub struct LocalAsk {
    pub endpoint: String,
    pub model: String,
    pub system: String,
    pub max_tokens: u64,
    pub temperature: f64,
    /// `Authorization: Bearer …` for a server that wants one — a literal, or
    /// `env:NAME` to read it from the environment so the value never has to
    /// live in a file that gets committed.
    pub key: String,
}

#[derive(Debug, Clone)]
pub struct Quicklink {
    pub title: String,
    pub subtitle: String,
    pub keyword: String,
    pub tags: Vec<String>,
    pub url: String,
    pub open: String,
    pub glyph: String,
}

impl Quicklink {
    /// `{}` is the argument. A link without one ignores whatever was typed —
    /// and the check is on the target `expand` would pick, `url` before
    /// `open`, so a placeholder hiding in an unused `open` does not pretend
    /// to take one.
    pub fn takes_argument(&self) -> bool {
        if self.url.is_empty() {
            self.open.contains("{}")
        } else {
            self.url.contains("{}")
        }
    }

    /// An empty argument would leave a bare `.../search?q=`, which is a worse
    /// page than the site root — trim the placeholder and everything after it.
    pub fn expand(&self, argument: &str) -> String {
        let target = if self.url.is_empty() {
            &self.open
        } else {
            &self.url
        };
        if !target.contains("{}") {
            return target.clone();
        }
        if argument.is_empty() {
            return target
                .split("{}")
                .next()
                .unwrap_or("")
                .trim_end_matches(['?', '&', '/'])
                .to_string();
        }
        target.replace("{}", &url_encode(argument))
    }

    /// The command Enter runs: `open` verbatim, or the browser on the url.
    pub fn command(&self, argument: &str) -> String {
        if !self.open.is_empty() {
            return if self.open.contains("{}") {
                self.expand(argument)
            } else {
                self.open.clone()
            };
        }
        let expanded = self.expand(argument);
        if expanded.is_empty() {
            return String::new();
        }
        format!(
            "omarchy-launch-browser {}",
            crate::support::quote::quote(&expanded)
        )
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub raw: Map<String, Value>,
    /// The last few things you typed, shown on an empty box. Off by default.
    pub recents: bool,
    /// `false` turns frecency ranking off. Absent means on.
    pub frecency: bool,
    /// `false` turns the event log off. Absent means on.
    pub log: bool,
    pub default_engine: String,
    pub engines: Vec<Engine>,
    /// Which engines appear as actions, in this order.
    pub engine_actions: Vec<String>,
    pub quicklinks: Vec<Quicklink>,
    /// Force one `askProviders` entry by id; empty leaves the list order to
    /// decide.
    pub ask_provider: String,
    /// Ctrl+Enter's backends, probed in order at ask time.
    pub ask_providers: Vec<AskProvider>,
    /// A local model server, when one is configured. Answering through it
    /// skips the CLI list entirely.
    pub ask: LocalAsk,
    /// Built-in extensions set to `false` stay off.
    pub extensions: Map<String, Value>,
    /// What each extension was configured with, by extension id.
    pub extension_settings: Map<String, Value>,
    /// Files this load had to move aside because they did not parse. Not a
    /// setting: the engine reports them once and moves on.
    pub recovered: Vec<std::path::PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            raw: Map::new(),
            recents: false,
            frecency: true,
            log: true,
            default_engine: "google".to_string(),
            engines: default_engines(),
            engine_actions: ["google", "chatgpt", "ddg", "youtube", "github"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            quicklinks: Vec::new(),
            ask_provider: String::new(),
            ask_providers: default_ask_providers(),
            ask: default_local_ask(),
            extensions: Map::new(),
            extension_settings: Map::new(),
            recovered: Vec::new(),
        }
    }
}

impl Settings {
    /// Merge a parsed `oxy.json` over the defaults. Unknown keys are kept in
    /// `raw` so nothing a user wrote is lost to the reader.
    pub fn merge(raw: &Value) -> Settings {
        let mut out = Settings::default();
        let Some(obj) = raw.as_object() else {
            return out;
        };
        out.raw = obj.clone();

        if let Some(v) = obj.get("recents").and_then(|v| v.as_bool()) {
            out.recents = v;
        }
        if let Some(v) = obj.get("frecency").and_then(|v| v.as_bool()) {
            out.frecency = v;
        }
        if let Some(v) = obj.get("log").and_then(|v| v.as_bool()) {
            out.log = v;
        }
        if let Some(v) = obj.get("defaultEngine").and_then(|v| v.as_str()) {
            out.default_engine = v.to_string();
        }
        if let Some(engines) = obj.get("engines").and_then(|v| v.as_array()) {
            let parsed: Vec<Engine> = engines
                .iter()
                .filter_map(|e| {
                    Some(Engine {
                        id: e.get("id")?.as_str()?.to_string(),
                        title: e
                            .get("title")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string(),
                        url: e.get("url")?.as_str()?.to_string(),
                    })
                })
                .collect();
            if !parsed.is_empty() {
                // The user's entries merge over the built-ins by id — adding
                // one engine does not mean restating Google, DuckDuckGo, …
                for engine in parsed {
                    match out.engines.iter_mut().find(|e| e.id == engine.id) {
                        Some(slot) => *slot = engine,
                        None => out.engines.push(engine),
                    }
                }
            }
        }
        if let Some(list) = obj.get("engineActions") {
            // The key's presence replaces — an empty list is "no engine
            // actions", not "keep the defaults" (the script copied it
            // verbatim, `root.config.engineActions || []`).
            out.engine_actions = str_array(Some(list));
        }
        if let Some(links) = obj.get("quicklinks").and_then(|v| v.as_array()) {
            out.quicklinks = links
                .iter()
                .filter_map(|l| {
                    let title = l.get("title").and_then(|t| t.as_str()).unwrap_or("");
                    let url = l.get("url").and_then(|t| t.as_str()).unwrap_or("");
                    let open = l.get("open").and_then(|t| t.as_str()).unwrap_or("");
                    if title.is_empty() || (url.is_empty() && open.is_empty()) {
                        return None;
                    }
                    Some(Quicklink {
                        title: title.to_string(),
                        subtitle: l
                            .get("subtitle")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string(),
                        keyword: l
                            .get("keyword")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_lowercase(),
                        tags: str_array(l.get("tags")),
                        url: url.to_string(),
                        open: open.to_string(),
                        glyph: l
                            .get("glyph")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect();
        }
        if let Some(v) = obj.get("askProvider").and_then(|v| v.as_str()) {
            out.ask_provider = v.to_string();
        }
        if let Some(list) = obj.get("askProviders").and_then(|v| v.as_array()) {
            let parsed: Vec<AskProvider> = list
                .iter()
                .filter_map(|p| {
                    Some(AskProvider {
                        id: p.get("id")?.as_str()?.to_string(),
                        title: p
                            .get("title")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string(),
                        model: p
                            .get("model")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string(),
                        command: p.get("command")?.as_str()?.to_string(),
                        when: p
                            .get("when")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect();
            // The key's presence replaces — an empty list is "ask nothing",
            // not "keep the defaults".
            out.ask_providers = parsed;
        }
        if let Some(a) = obj.get("ask").and_then(|v| v.as_object()) {
            let s = |k: &str| a.get(k).and_then(|v| v.as_str());
            if let Some(v) = s("endpoint") {
                out.ask.endpoint = v.to_string();
            }
            if let Some(v) = s("model") {
                out.ask.model = v.to_string();
            }
            if let Some(v) = s("system") {
                out.ask.system = v.to_string();
            }
            if let Some(v) = a.get("maxTokens").and_then(|v| v.as_u64()) {
                out.ask.max_tokens = v;
            }
            if let Some(v) = a.get("temperature").and_then(|v| v.as_f64()) {
                out.ask.temperature = v;
            }
            if let Some(v) = s("key") {
                out.ask.key = v.to_string();
            }
        }
        if let Some(v) = obj.get("extensions").and_then(|v| v.as_object()) {
            out.extensions = v.clone();
        }
        if let Some(v) = obj.get("extensionSettings").and_then(|v| v.as_object()) {
            out.extension_settings = v.clone();
        }
        out
    }

    /// Read `oxy.json`. A file that does not parse is moved aside (see
    /// `support::store`) and its path lands in `recovered` for the caller to
    /// report — the settings fall back to defaults either way, but the file
    /// survives for the human who wrote it.
    pub fn load(path: &Path) -> Settings {
        let mut recovered = Vec::new();
        let raw = crate::support::store::read_json::<Value>(path, &mut recovered);
        let mut out = raw.map(|v| Settings::merge(&v)).unwrap_or_default();
        out.recovered = recovered;
        out
    }

    /// The `extensionSettings.<id>` object for one extension, for the
    /// environment prefix its command gets.
    pub fn settings_for(&self, id: &str) -> Option<&Map<String, Value>> {
        self.extension_settings.get(id)?.as_object()
    }

    pub fn engine(&self, id: &str) -> Option<&Engine> {
        self.engines.iter().find(|e| e.id == id)
    }

    /// The providers to try, in order: an explicit `askProvider` leads, and
    /// an id that matches nothing is a typo, not an instruction to ask
    /// nothing — so the full list is the fallback.
    pub fn ask_ordered(&self) -> Vec<&AskProvider> {
        let mut ordered: Vec<&AskProvider> = Vec::new();
        if !self.ask_provider.is_empty() {
            ordered.extend(
                self.ask_providers
                    .iter()
                    .filter(|p| p.id == self.ask_provider),
            );
            if !ordered.is_empty() {
                return ordered;
            }
        }
        self.ask_providers.iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `engines` in oxy.json merge over the defaults by id: adding Kagi must
    /// not make Google disappear, and a same-id entry overrides in place —
    /// the QML semantics, which replacing the list did not share.
    #[test]
    fn the_ask_block_merges_field_by_field() {
        let s = Settings::merge(&serde_json::json!({
            "ask": {"endpoint": "http://127.0.0.1:11434/v1/chat/completions",
                    "model": "llama3.2", "key": "env:OPENAI_API_KEY", "maxTokens": 64}
        }));
        assert_eq!(s.ask.model, "llama3.2");
        assert_eq!(s.ask.key, "env:OPENAI_API_KEY");
        assert_eq!(s.ask.max_tokens, 64);
        // Untouched fields keep their defaults.
        assert_eq!(s.ask.temperature, 0.4);
        assert!(!s.ask.system.is_empty());
        // No `ask` block at all leaves the endpoint empty, so the CLI answers.
        let bare = Settings::merge(&serde_json::json!({}));
        assert!(bare.ask.endpoint.is_empty());
        assert!(bare.ask.key.is_empty());
    }

    #[test]
    fn a_corrupt_settings_file_is_moved_aside_and_reported() {
        let d = std::env::temp_dir().join(format!("oxy-settings-{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("oxy.json");
        std::fs::write(&path, r#"{"recents": true,"#).unwrap(); // cut mid-write

        let s = Settings::load(&path);
        assert_eq!(s.recovered, vec![d.join("oxy.corrupt")]);
        assert!(!s.recents, "defaults, and the file survives to be fixed");
        assert!(d.join("oxy.corrupt").exists());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn engines_merge_by_id() {
        let s = Settings::merge(&serde_json::json!({
            "engines": [
                {"id": "kagi", "title": "Kagi", "url": "https://kagi.com/search?q={}"},
                {"id": "google", "title": "G", "url": "https://g.example/{}"}
            ]
        }));
        let kagi = s.engine("kagi").expect("added engine");
        assert_eq!(kagi.title, "Kagi");
        let google = s.engine("google").expect("default engine survives");
        assert_eq!(google.url, "https://g.example/{}");
        assert!(s.engine("ddg").is_some());
    }
}
