//! Applications, found the freedesktop way: every `*.desktop` under the XDG
//! applications dirs, parsed, with `Hidden`/`NoDisplay`/`OnlyShowIn`/`NotShowIn`
//! and the `launcher.hides` list honoured.
//!
//! Scored by the same fuzzy function everything else is, so a prefix match on
//! an app and a prefix match on a command mean the same thing.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use super::apps_icons::{IconIndex, apps_fingerprint, resolve_icon, scan_icons};
use crate::model::row::{Action, Row};
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote as shellquote;
use crate::support::rank;
use crate::support::score::{Entry, fuzzy};

pub(super) struct App {
    pub(super) entry: Entry,
    pub(super) icon: String,
}

/// The scan result behind a lock: the walk over every `applications` dir is
/// filesystem work, so the query body runs on `spawn_blocking`.
pub struct Apps {
    state: std::sync::Arc<std::sync::Mutex<Scan>>,
}

#[derive(Default)]
struct Scan {
    apps: Vec<App>,
    icons: IconIndex,
    scanned: bool,
    /// The entry set the icon index was built against. The QML re-scanned
    /// the icon dirs off `DesktopEntries.onValuesChanged` — here the same
    /// edge is a changed fingerprint of the `.desktop` set: a package
    /// install or removal alters both, and a fresh summon that finds the
    /// same entries keeps the index it already paid for.
    icons_fp: u64,
}

impl Default for Apps {
    fn default() -> Self {
        Self::new()
    }
}

impl Apps {
    pub fn new() -> Apps {
        Apps {
            state: std::sync::Arc::new(std::sync::Mutex::new(Scan::default())),
        }
    }
}

impl NativeExt for Apps {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let state = self.state.clone();
        let arg = ctx.arg.clone();
        let unscoped = ctx.query.scope.is_empty();
        let fresh = ctx.fresh_open;
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let mut scan = state.lock().unwrap();
                // First ask, or the first ask of a fresh open: whatever was
                // installed while the launcher was away is worth one rescan.
                if !scan.scanned || fresh {
                    let apps = scan_applications();
                    let fp = apps_fingerprint(&apps);
                    if !scan.scanned || fp != scan.icons_fp {
                        scan.icons = scan_icons();
                        scan.icons_fp = fp;
                    }
                    scan.apps = apps;
                    scan.scanned = true;
                }
                let query = arg.trim();
                if query.is_empty() && unscoped {
                    return NativeOutcome::Empty;
                }

                let mut scored: Vec<(i64, &App)> = Vec::new();
                for app in &scan.apps {
                    let score = fuzzy(&app.entry, query);
                    if score < 0 {
                        continue;
                    }
                    scored.push((score, app));
                }
                // Empty query is alphabetical; a real query is by score then name.
                scored.sort_by(|a, b| {
                    if !query.is_empty() && a.0 != b.0 {
                        return b.0.cmp(&a.0);
                    }
                    a.1.entry
                        .name
                        .to_lowercase()
                        .cmp(&b.1.entry.name.to_lowercase())
                });
                scored.truncate(20);

                let rows: Vec<Row> = scored
                    .into_iter()
                    .map(|(fuzzy_score, app)| {
                        let name = app.entry.name.clone();
                        let id = app.entry.id.clone();
                        let launch = format!("uwsm-app -- gtk-launch {}", shellquote::quote(&id));
                        let mut row = Row::new(format!("app:{id}"), "apps");
                        row.group = "Applications".into();
                        row.title = name.clone();
                        row.subtitle = app.entry.generic_name.clone();
                        row.icon_source = resolve_icon(&app.icon, &scan.icons);
                        row.extra.insert("copyText".into(), json!(name));
                        row.tier = rank::tier_for_fuzzy(fuzzy_score);
                        row.local = rank::local_for_fuzzy(fuzzy_score);
                        row.score = rank::score(row.tier, row.local, 0);
                        row.exec = launch.clone();
                        row.actions = Some(vec![
                            Action {
                                title: "Open".into(),
                                shortcut: "↵".into(),
                                exec: launch,
                                ..Action::default()
                            },
                            Action {
                                title: "Copy Name".into(),
                                exec: format!("printf %s {} | wl-copy", shellquote::quote(&name)),
                                ..Action::default()
                            },
                        ]);
                        row
                    })
                    .collect();
                NativeOutcome::Built(rows)
            })
            .await
            .unwrap_or(NativeOutcome::Empty)
        })
    }
}

// --------------------------------------------------------------- the scan

fn applications_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![crate::settings::paths::data_home().join("applications")];
    for base in std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string())
        .split(':')
        .filter(|d| !d.is_empty())
    {
        dirs.push(Path::new(base).join("applications"));
    }
    // Flatpak and Snap install here on systems that have them.
    for extra in [
        crate::settings::paths::home().join(".local/share/flatpak/exports/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
        PathBuf::from("/var/lib/snapd/desktop/applications"),
    ] {
        dirs.push(extra);
    }
    dirs
}

/// The desktop id for a file: the path relative to its applications dir with
/// `/` turned into `-`, so `foo/bar.desktop` is `foo-bar.desktop`.
fn desktop_id(dir: &Path, path: &Path) -> String {
    path.strip_prefix(dir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('/', "-")
}

/// The ids `launcher.hides` names. Same file the QML fallback read, from the
/// same two places.
fn hidden_ids() -> HashSet<String> {
    let mut out = HashSet::new();
    let mut roots = Vec::new();
    if let Ok(omarchy) = std::env::var("OMARCHY_PATH") {
        roots.push(PathBuf::from(omarchy));
    }
    roots.push(crate::settings::paths::data_home().join("omarchy"));
    for root in roots {
        let path = root.join("default/omarchy/launcher.hides");
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                let id = line.trim().trim_end_matches(".desktop");
                if !id.is_empty() {
                    out.insert(id.to_string());
                }
            }
        }
    }
    out
}

fn current_desktops() -> HashSet<String> {
    let mut out = HashSet::new();
    for var in [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
    ] {
        if let Ok(value) = std::env::var(var) {
            for d in value.split(':') {
                if !d.is_empty() {
                    out.insert(d.to_string());
                }
            }
        }
    }
    out
}

fn list_contains(list: &str, desktops: &HashSet<String>) -> bool {
    list.split(';')
        .filter(|d| !d.is_empty())
        .any(|d| desktops.contains(d))
}

struct DesktopEntry {
    name: String,
    generic_name: String,
    comment: String,
    keywords: Vec<String>,
    icon: String,
    no_display: bool,
    hidden: bool,
    only_show_in: String,
    not_show_in: String,
    entry_type: String,
}

fn parse_desktop(text: &str, locale: &str) -> Option<DesktopEntry> {
    let mut in_entry = false;
    let mut found = false;
    let mut fields: HashMap<String, String> = HashMap::new();
    // A localized Name beats the plain one when the machine's locale matches.
    let localized = format!("Name[{locale}]");

    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        found = true;
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        // The localized name only replaces a plain Name read, never the other
        // way: whichever order the file lists them in, the better one wins.
        if key == localized.as_str() || (key == "Name" && !fields.contains_key(&localized)) {
            fields.insert("Name".to_string(), value.trim().to_string());
            continue;
        }
        fields
            .entry(key.to_string())
            .or_insert_with(|| value.trim().to_string());
    }
    if !found {
        return None;
    }
    Some(DesktopEntry {
        name: fields.get("Name").cloned().unwrap_or_default(),
        generic_name: fields.get("GenericName").cloned().unwrap_or_default(),
        comment: fields.get("Comment").cloned().unwrap_or_default(),
        keywords: fields
            .get("Keywords")
            .map(|k| {
                k.split(';')
                    .filter(|w| !w.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        icon: fields.get("Icon").cloned().unwrap_or_default(),
        no_display: fields.get("NoDisplay").is_some_and(|v| v == "true"),
        hidden: fields.get("Hidden").is_some_and(|v| v == "true"),
        only_show_in: fields.get("OnlyShowIn").cloned().unwrap_or_default(),
        not_show_in: fields.get("NotShowIn").cloned().unwrap_or_default(),
        entry_type: fields.get("Type").cloned().unwrap_or_default(),
    })
}

fn locale() -> String {
    for var in ["LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(var) {
            // "pt_BR.UTF-8" → "pt_BR"
            let lang = value.split('.').next().unwrap_or("");
            if !lang.is_empty() && lang != "C" && lang != "POSIX" {
                return lang.to_string();
            }
        }
    }
    String::new()
}

fn scan_applications() -> Vec<App> {
    let hidden = hidden_ids();
    let desktops = current_desktops();
    let lang = locale();
    let mut seen: HashSet<String> = HashSet::new();
    let mut apps = Vec::new();

    for dir in applications_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        // Later dirs shadow earlier ones by desktop id, the way XDG_DATA_HOME
        // overrides XDG_DATA_DIRS.
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let id = desktop_id(&dir, &path);
            if seen.contains(&id) {
                continue;
            }
            seen.insert(id.clone());

            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(parsed) = parse_desktop(&text, &lang) else {
                continue;
            };
            if parsed.entry_type != "Application" || parsed.name.is_empty() {
                continue;
            }
            if parsed.no_display || parsed.hidden {
                continue;
            }
            if hidden.contains(id.trim_end_matches(".desktop")) {
                continue;
            }
            if !parsed.only_show_in.is_empty() && !list_contains(&parsed.only_show_in, &desktops) {
                continue;
            }
            if !parsed.not_show_in.is_empty() && list_contains(&parsed.not_show_in, &desktops) {
                continue;
            }

            apps.push(App {
                entry: Entry::new(
                    id.clone(),
                    parsed.name,
                    parsed.generic_name,
                    parsed.comment,
                    parsed.keywords,
                ),
                icon: parsed.icon,
            });
        }
    }
    apps
}
