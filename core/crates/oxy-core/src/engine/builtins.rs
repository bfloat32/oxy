//! Built-in data: the scopes, keywords, synthesized extensions and actions
//! the launcher answers for itself.

use std::collections::HashSet;

use crate::model::row::Action;
use crate::registry::Extension;
use crate::support::rank;

pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ------------------------------------------------------------ builtin data

/// The four built-in scopes, as `Help.js` listed them.
pub(super) fn builtin_help() -> &'static [(&'static str, &'static str, &'static [&'static str])] {
    &[
        ("calc", "Calculator", &["math"]),
        ("run", "Commands", &["command", "commands"]),
        ("apps", "Applications", &["app", "launch"]),
        ("web", "Web Search", &["search", "google", "ddg"]),
        ("h", "Keywords", &["help", "?", ":"]),
    ]
}

/// What the parser knows before the registry has spoken.
pub(super) fn builtin_keywords() -> HashSet<String> {
    [
        "calc", "math", "run", "command", "commands", "web", "search", "google", "ddg", "apps",
        "app", "launch", "settings", "action", "h", "help",
        // Extra filters the built-ins read alongside their own keyword.
        "format", "in", "type",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// The built-ins, declared as extensions so one set of rules routes them —
/// including the worker's debounce/timeout/cache machinery.
///
/// `links` supplies the quicklink keywords: a link's own keyword addresses
/// it — `later:hi` scopes to `later` and the provider answers for that link,
/// the way `Quicklinks.js` treated `query.scope === link.keyword`. The
/// keywords ride on the extension as aliases, so one routing table covers
/// both the engine's `claims` and the worker's gate — and a quicklinks edit
/// is an `aliases` change, which the worker reconcile picks up.
pub(super) fn builtin_extensions(links: &[crate::settings::Quicklink]) -> Vec<Extension> {
    let mut out = Vec::new();
    let synth = |id: &str,
                 title: &str,
                 keyword: &str,
                 aliases: &[&str],
                 native: &str,
                 always: bool,
                 min_chars: usize,
                 debounce: u64,
                 max_rows: usize,
                 tier: &str,
                 view: &str,
                 builtin: bool| {
        Extension {
            id: id.to_string(),
            title: title.to_string(),
            keyword: keyword.to_string(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            filters: vec![],
            search: String::new(),
            when: String::new(),
            glyph: String::new(),
            subtitle: title.to_string(),
            min_chars,
            debounce_ms: debounce,
            timeout_ms: 4000,
            max_rows,
            tier: rank::tier(tier),
            view: view.to_string(),
            always,
            cache_ms: 0,
            refresh_ms: 0,
            socket: String::new(),
            native: native.to_string(),
            actions: vec![],
            accent: String::new(),
            settings: vec![],
            test_query: String::new(),
            source: std::path::PathBuf::from("<builtin>"),
            builtin,
        }
    };
    out.push(synth(
        "calc",
        "Calculator",
        "calc",
        &["math"],
        "calc",
        true,
        1,
        90,
        4,
        "calc",
        "hero",
        true,
    ));
    // `min_chars: 0` makes the bare keyword a browse mode — `apps:` lists
    // apps alphabetically, `run:` lists every command — the way the script
    // build's scoped query functions behaved on an empty argument. The
    // providers still decline a truly empty query (`query.empty`), so the
    // unscoped empty box stays recents-only.
    out.push(synth(
        "apps",
        "Applications",
        "apps",
        &["app", "launch"],
        "apps",
        true,
        0,
        0,
        20,
        "substring",
        "list",
        true,
    ));
    // The script build capped commands and quicklinks only at the merge
    // limit — all 24 commands and every link are reachable.
    out.push(synth(
        "commands",
        "Commands",
        "run",
        &["commands"],
        "commands",
        true,
        0,
        0,
        60,
        "substring",
        "list",
        true,
    ));
    out.push(synth(
        "quicklinks",
        "Quicklinks",
        "quicklinks",
        &[],
        "quicklinks",
        true,
        0,
        0,
        60,
        "substring",
        "list",
        true,
    ));
    if let Some(ql) = out.iter_mut().find(|e| e.id == "quicklinks") {
        ql.aliases = links
            .iter()
            .map(|l| l.keyword.to_lowercase())
            .filter(|k| !k.is_empty())
            .collect();
    }
    out.push(synth(
        "web",
        "Web",
        "web",
        &["search", "google", "ddg"],
        "web",
        true,
        1,
        0,
        1,
        "web",
        "list",
        true,
    ));
    out
}

/// The built-in actions, as `Actions.js` declared them.
pub(super) fn builtin_actions() -> Vec<Action> {
    let mk =
        |id: &str, title: &str, subtitle: &str, effect: &str, confirm: &str, keywords: &[&str]| {
            Action {
                id: id.to_string(),
                title: title.to_string(),
                subtitle: subtitle.to_string(),
                effect: effect.to_string(),
                confirm: confirm.to_string(),
                keywords: keywords.iter().map(|s| s.to_string()).collect(),
                ..Action::default()
            }
        };
    let mut out = vec![
        mk(
            "clear",
            "Clear Recent Queries",
            "History",
            "clear.recents",
            "",
            &["clear", "recent", "recents", "history", "forget"],
        ),
        mk(
            "clear-pins",
            "Clear Pinned Results",
            "History",
            "clear.pins",
            "",
            &["clear", "pins", "pinned", "unpin"],
        ),
        // Recents come back on their own; pins do not. One keypress should
        // not throw a pin away silently.
        mk(
            "clear-all",
            "Clear Everything",
            "History",
            "clear.all",
            "Clear recent queries and every pin?",
            &["clear", "all", "reset", "everything", "wipe"],
        ),
        mk(
            "reload",
            "Reload Extensions",
            "Launcher",
            "reload.extensions",
            "",
            &["reload", "refresh", "extensions", "rescan"],
        ),
        mk(
            "settings",
            "Extension Settings",
            "Launcher",
            "open.settings",
            "",
            &["settings", "config", "preferences", "options"],
        ),
        mk(
            "stats",
            "Launcher Stats",
            "Launcher",
            "stats",
            "",
            &["stats", "statistics", "diagnostics", "cache"],
        ),
    ];
    let mut logs = mk(
        "logs",
        "Open Event Log",
        "Launcher",
        "",
        "",
        &["log", "logs", "events", "diagnostics", "debug", "trace"],
    );
    logs.exec = "omarchy-launch-editor ~/.local/state/omarchy/oxy-log.jsonl".into();
    out.push(logs);
    let mut config = mk(
        "config",
        "Edit oxy.json",
        "Launcher",
        "",
        "",
        &["config", "json", "edit", "file"],
    );
    config.exec = "omarchy-launch-editor ~/.config/omarchy/oxy.json".into();
    out.push(config);
    out
}
