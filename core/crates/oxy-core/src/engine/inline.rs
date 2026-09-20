//! Inline answers: the launcher's own providers — help, recents, paste,
//! settings, actions — answered synchronously in the engine's pass.

use std::collections::HashSet;

use serde_json::{Value, json};

use super::Engine;
use super::builtins::{builtin_actions, builtin_help};
use crate::model::query::Query;
use crate::model::row::{Action, Row};
use crate::registry::Extension;
use crate::support::rank;

impl Engine {
    // ------------------------------------------------------- inline answers

    /// `?` on its own, `h:`, `help:` or a bare `:`: every keyword the
    /// launcher knows, in the order a reader expects.
    pub(super) fn answer_help(&self, query: &Query) -> Vec<Row> {
        let t = self.raw.trim();
        let help = matches!(t, "?" | ":" | "h:" | "help:")
            || (query.scope == "h" || query.scope == "help") && query.text.is_empty();
        if !help {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        let mut push =
            |group: &str, keyword: &str, title: &str, aliases: Vec<String>, glyph: &str| {
                let name = keyword.to_lowercase();
                if name.is_empty() || !seen.insert(name.clone()) {
                    return;
                }
                let mut row = fill_row(FillSpec {
                    provider: "help",
                    key: &format!("help:{name}"),
                    group,
                    title,
                    subtitle: &aliases.join(", "),
                    accessory: &format!("{name}:"),
                    glyph,
                    fill: &format!("{name}:"),
                    verb: "Use Keyword",
                });
                // Listed in the order the list was built, so the rank only
                // preserves it.
                row.score = rank::score(rank::TIER_FORCED, 90000 - out.len() as i64 * 200, 0);
                out.push(row);
            };

        // Built-ins first, then extensions, then quicklinks: most general to
        // most personal.
        for (kw, title, aliases) in builtin_help() {
            let mut alias = aliases.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            for s in crate::model::query::sigils_for(kw) {
                alias.push(s.to_string());
            }
            push("Built In", kw, title, alias, "");
        }
        push("Built In", "settings", "Settings", vec![], "");
        for ext in self.extensions.iter() {
            if ext.builtin {
                continue;
            }
            let mut alias = ext.aliases.clone();
            for s in crate::model::query::sigils_for(&ext.keyword) {
                alias.push(s.to_string());
            }
            push("Extensions", &ext.keyword, &ext.title, alias, &ext.glyph);
        }
        for link in &self.settings.quicklinks {
            if link.keyword.is_empty() {
                continue;
            }
            push(
                "Quicklinks",
                &link.keyword,
                &link.title,
                vec![],
                &link.glyph,
            );
        }
        out
    }

    /// What you ran last, on an empty box — off unless `recents` is set.
    pub(super) fn answer_recents(&self, _query: &Query) -> Vec<Row> {
        if !(self.settings.recents && self.raw.trim().is_empty()) {
            return Vec::new();
        }
        self.state
            .recents
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let mut row = fill_row(FillSpec {
                    provider: "recents",
                    key: &format!("past:{entry}"),
                    group: "Recent",
                    title: entry,
                    subtitle: "",
                    accessory: "",
                    glyph: "",
                    fill: entry,
                    verb: "Search Again",
                });
                row.score = rank::score(rank::TIER_FORCED, 90000 - i as i64 * 200, 0);
                row
            })
            .collect()
    }

    /// A URL on the clipboard, offered as the first row of an empty box.
    pub(super) fn answer_paste(&self, _query: &Query) -> Vec<Row> {
        let Some(url) = self.clipboard_url.clone() else {
            return Vec::new();
        };
        if !(self.settings.recents && self.raw.trim().is_empty()) {
            return Vec::new();
        }
        let mut row = Row::new(format!("paste:{url}"), "paste");
        row.group = "Clipboard".into();
        row.title = url.clone();
        row.detail = "On the clipboard".into();
        row.accessory = "Open".into();
        // A link glyph, so the row reads as something you copied before the
        // URL itself has been read at all.
        row.icon_glyph = "\u{f0c1}".into();
        row.score = rank::score(rank::TIER_FORCED, 95000, 0);
        row.exec = format!(
            "omarchy-launch-browser {}",
            crate::support::quote::quote(&url)
        );
        let search = self
            .settings
            .engine(&self.settings.default_engine)
            .map(|e| e.url.replace("{}", &crate::settings::url_encode(&url)))
            .unwrap_or_default();
        row.actions = Some(vec![
            Action {
                title: "Open Link".into(),
                shortcut: "↵".into(),
                exec: row.exec.clone(),
                ..Action::default()
            },
            Action {
                title: "Search For It".into(),
                exec: format!(
                    "omarchy-launch-browser {}",
                    crate::support::quote::quote(&search)
                ),
                ..Action::default()
            },
        ]);
        vec![row]
    }

    /// `settings:` — extensions that declared fields, then the form for the
    /// one picked.
    pub(super) async fn answer_settings(&self, query: &Query) -> Vec<Row> {
        if query.scope != "settings" {
            return Vec::new();
        }
        let arg = query.arg_for("settings", &[]).trim().to_lowercase();

        // A picked extension with fields is a form, not a list.
        if let Some(ext) = self.extensions.iter().find(|e| e.id == arg)
            && !ext.settings.is_empty()
        {
            return vec![self.settings_form(ext)];
        }

        self.extensions
            .iter()
            .filter(|ext| !ext.settings.is_empty())
            .filter(|ext| {
                arg.is_empty()
                    || ext.id.starts_with(&arg)
                    || ext.title.to_lowercase().contains(&arg)
            })
            .enumerate()
            .map(|(i, ext)| {
                let saved = self.settings.settings_for(&ext.id);
                let filled = ext
                    .settings
                    .iter()
                    .filter(|s| {
                        saved
                            .and_then(|m| m.get(&s.key))
                            .and_then(|v| v.as_str())
                            .is_some_and(|v| !v.is_empty())
                    })
                    .count();
                let mut row = Row::new(format!("settings:{}", ext.id), "settings");
                row.group = "Settings".into();
                row.title = ext.title.clone();
                row.detail = format!("{} of {} set", filled, ext.settings.len());
                row.accessory = format!("{}:", ext.keyword);
                row.icon_glyph = ext.glyph.clone();
                row.score = rank::score(rank::TIER_FORCED, 90000 - i as i64 * 200, 0);
                // A query and nothing else: choosing one is a step further
                // in, and Escape is what undoes it.
                let mut act = Action {
                    title: "Edit".into(),
                    shortcut: "↵".into(),
                    ..Action::default()
                };
                act.extra
                    .insert("query".into(), json!(format!("settings:{}", ext.id)));
                row.actions = Some(vec![act]);
                row
            })
            .collect()
    }

    /// The form row for one extension.
    fn settings_form(&self, ext: &Extension) -> Row {
        let saved = self.settings.settings_for(&ext.id);
        let fields: Vec<Value> = ext
            .settings
            .iter()
            .filter(|s| !s.key.is_empty())
            .map(|s| {
                // What is saved wins over what the extension suggested: the
                // suggestion is a default, and a default that overwrote an
                // answer would not be one.
                let value = saved
                    .and_then(|m| m.get(&s.key))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&s.value)
                    .to_string();
                json!({
                    "name": s.key,
                    "label": if s.label.is_empty() { &s.key } else { &s.label },
                    "value": value,
                    "placeholder": s.placeholder,
                    "secret": s.secret,
                })
            })
            .collect();

        let mut row = Row::new(format!("settings:form:{}", ext.id), "settings");
        row.group = "Settings".into();
        row.view = "form".into();
        row.title = ext.title.clone();
        row.subtitle = "Saved to ~/.config/omarchy/oxy.json".into();
        row.extra.insert("submit".into(), json!("Save"));
        row.extra.insert("fields".into(), json!(fields));
        row.extra.insert("ext".into(), json!(ext.id));
        let mut act = Action::default();
        act.extra.insert("query".into(), json!("settings:"));
        row.actions = Some(vec![act]);
        row.score = rank::score(rank::TIER_FORCED, 99000, 0);
        row
    }

    /// `/` — everything the launcher can do to itself, plus the actions
    /// extensions declare.
    pub(super) fn answer_actions(&self, query: &Query) -> Vec<Row> {
        if query.scope != "command" {
            return Vec::new();
        }
        let arg = query.arg_for("command", &[]).trim().to_lowercase();
        let mut out = Vec::new();

        let all = self.all_actions();
        for (i, action) in all.iter().enumerate() {
            let entry = crate::support::score::Entry {
                id: String::new(),
                name: action.title.clone(),
                generic_name: action.subtitle.clone(),
                comment: String::new(),
                keywords: action.keywords.clone(),
                payload: Value::Null,
            };
            let fuzzy = if arg.is_empty() {
                0
            } else {
                crate::support::score::fuzzy(&entry, &arg)
            };
            // A confirm armed on this action keeps it in the list even though
            // the retyped `/id` no longer matches its keywords.
            let confirming = self
                .pending_confirm
                .as_ref()
                .is_some_and(|id| *id == action.id);
            if fuzzy < 0 && !confirming {
                continue;
            }

            let mut row = Row::new(format!("action:{}", action.id), "actions");
            row.group = "Commands".into();
            row.title = action.title.clone();
            row.subtitle = action.subtitle.clone();
            row.icon_glyph = action.glyph.clone();
            row.extra
                .insert("keepOpen".into(), json!(action.exec.is_empty()));
            // Declared order always — a fuzzy score is not a better one.
            row.tier = rank::TIER_FORCED;
            row.local = 900 - i as i64;
            row.score = rank::score(rank::TIER_FORCED, row.local, 0);
            let mut act = action.clone();
            act.shortcut = "↵".into();
            row.actions = Some(vec![act]);
            out.push(row);
        }
        out
    }

    /// The built-in actions plus each extension's, namespaced by id.
    pub(super) fn all_actions(&self) -> Vec<Action> {
        let mut out = builtin_actions();
        for ext in self.extensions.iter() {
            for (j, a) in ext.actions.iter().enumerate() {
                if a.title.is_empty() {
                    continue;
                }
                let mut action = a.clone();
                action.id = format!(
                    "{}.{}",
                    ext.id,
                    if a.id.is_empty() {
                        j.to_string()
                    } else {
                        a.id.clone()
                    }
                );
                if action.subtitle.is_empty() {
                    action.subtitle = ext.title.clone();
                }
                if action.glyph.is_empty() {
                    action.glyph = ext.glyph.clone();
                }
                // The extension's id and the action's own id are keywords too,
                // so `/spotify auth` reaches it — and so does `/spotify.auth`,
                // which is what a `confirm` re-types.
                let mut kw = vec![ext.keyword.clone(), ext.id.clone(), a.id.clone()];
                kw.extend(a.keywords.clone());
                action.keywords = kw;
                out.push(action);
            }
        }
        out
    }
}

/// Everything a fill row needs — named fields so the two call sites stay
/// readable instead of juggling positional `&str`s.
struct FillSpec<'a> {
    provider: &'a str,
    key: &'a str,
    group: &'a str,
    title: &'a str,
    subtitle: &'a str,
    accessory: &'a str,
    glyph: &'a str,
    fill: &'a str,
    /// What Enter is called — "Use Keyword" for help rows, "Search Again"
    /// for recents, matching `Launcher.qml`'s two fill sites.
    verb: &'a str,
}

/// A row whose only business is putting text in the box.
fn fill_row(spec: FillSpec<'_>) -> Row {
    let mut row = Row::new(spec.key, spec.provider);
    row.group = spec.group.into();
    row.title = spec.title.into();
    row.subtitle = spec.subtitle.into();
    row.accessory = spec.accessory.into();
    row.icon_glyph = spec.glyph.into();
    row.fill = spec.fill.into();
    // A self-reference, so the footer names what Enter does and Ctrl+K on the
    // row leads back through activate.
    let mut act = Action {
        title: spec.verb.into(),
        shortcut: "↵".into(),
        ..Action::default()
    };
    act.extra.insert("row".into(), json!(spec.key));
    row.actions = Some(vec![act]);
    row
}
